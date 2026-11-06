// Set Creator: an offline editor for GameShow question sets.
//
// Sets live as JSON files in the app data folder (`sets/<uuid>.json`), media files next to
// them (`media/<uuid>` + `media/<uuid>.json` with kind, name and type). The UI (plain HTML/JS
// in `ui/`) talks to the commands below; media is served to it via the `media:` URI scheme.
// A set is exported as a `.gameshow.zip` that GameShow imports.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::http::{Request, Response, StatusCode};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;
use uuid::Uuid;

/// Same as GameShow (MediaAsset.MaxBytes).
const MAX_MEDIA_BYTES: u64 = 50 * 1024 * 1024;
const MEDIA_NAME_MAX: usize = 200;
const MAX_JSON_BYTES: u64 = 2 * 1024 * 1024;
const MAX_ZIP_ENTRIES: usize = 300;
const MAX_ZIP_TOTAL: u64 = 1024 * 1024 * 1024;
/// Largest piece of a media file sent per range request (lets video seek without loading it all).
const MAX_RANGE_CHUNK: u64 = 8 * 1024 * 1024;
const SET_FORMAT: &str = "gameshow-set";
const SET_VERSION: u64 = 2;
/// Fields of a set file that are copied into a stored set (everything else is dropped).
const SET_FIELDS: [&str; 10] = [
    "title",
    "mode",
    "rowPoints",
    "categories",
    "stage2Cards",
    "lineBonuses",
    "stage3Questions",
    "stage4Questions",
    "timers",
    "soloQuestions",
];

type Res<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

// ---------- storage ----------

struct Dirs {
    sets: PathBuf,
    media: PathBuf,
}

fn dirs(app: &AppHandle) -> Res<Dirs> {
    let base = app.path().app_data_dir().map_err(err)?;
    let d = Dirs {
        sets: base.join("sets"),
        media: base.join("media"),
    };
    fs::create_dir_all(&d.sets).map_err(err)?;
    fs::create_dir_all(&d.media).map_err(err)?;
    Ok(d)
}

/// Parses an id and gives it back in canonical form, so it is always safe as a file name.
fn canonical_id(id: &str) -> Res<String> {
    Uuid::parse_str(id.trim())
        .map(|u| u.hyphenated().to_string())
        .map_err(|_| "err.badId".to_string())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Writes through a temp file + rename, so a crash never leaves a half-written file.
fn write_atomic(path: &Path, bytes: &[u8]) -> Res<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp).map_err(err)?;
        f.write_all(bytes).map_err(err)?;
        f.sync_all().map_err(err)?;
    }
    fs::rename(&tmp, path).map_err(err)
}

fn read_set(d: &Dirs, id: &str) -> Res<Value> {
    let id = canonical_id(id)?;
    let text = fs::read_to_string(d.sets.join(format!("{id}.json"))).map_err(|_| "err.notFound")?;
    serde_json::from_str(&text).map_err(err)
}

fn store_set(d: &Dirs, mut set: Value) -> Res<Value> {
    let id = canonical_id(set.get("id").and_then(Value::as_str).unwrap_or(""))?;
    let obj = set.as_object_mut().ok_or("err.badSet")?;
    obj.insert("id".into(), json!(id));
    obj.insert("updatedAt".into(), json!(now_ms()));
    let bytes = serde_json::to_vec_pretty(&set).map_err(err)?;
    write_atomic(&d.sets.join(format!("{id}.json")), &bytes)?;
    Ok(set)
}

/// Calls `f` on every `media` / `secondMedia` value anywhere in a set.
fn each_media_ref(value: &mut Value, f: &mut dyn FnMut(&mut Value)) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(|v| each_media_ref(v, f)),
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if key == "media" || key == "secondMedia" {
                    f(v);
                } else {
                    each_media_ref(v, f);
                }
            }
        }
        _ => {}
    }
}

fn media_ids(set: &Value) -> Vec<String> {
    let mut set = set.clone();
    let mut ids = Vec::new();
    each_media_ref(&mut set, &mut |v| {
        if let Some(id) = v.get("id").and_then(Value::as_str).and_then(|s| canonical_id(s).ok()) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    });
    ids
}

// ---------- media formats (mirrors GameShow MediaFormats; SVG left out on purpose) ----------

const CONTENT_TYPES: [(&str, &str); 16] = [
    ("image/jpeg", "Image"),
    ("image/png", "Image"),
    ("image/gif", "Image"),
    ("image/webp", "Image"),
    ("image/avif", "Image"),
    ("audio/mpeg", "Audio"),
    ("audio/ogg", "Audio"),
    ("audio/wav", "Audio"),
    ("audio/mp4", "Audio"),
    ("audio/aac", "Audio"),
    ("audio/webm", "Audio"),
    ("audio/flac", "Audio"),
    ("video/mp4", "Video"),
    ("video/webm", "Video"),
    ("video/ogg", "Video"),
    ("video/quicktime", "Video"),
];

const EXTENSIONS: [(&str, &str); 18] = [
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("png", "image/png"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("avif", "image/avif"),
    ("mp3", "audio/mpeg"),
    ("ogg", "audio/ogg"),
    ("oga", "audio/ogg"),
    ("wav", "audio/wav"),
    ("m4a", "audio/mp4"),
    ("aac", "audio/aac"),
    ("flac", "audio/flac"),
    ("mp4", "video/mp4"),
    ("m4v", "video/mp4"),
    ("webm", "video/webm"),
    ("ogv", "video/ogg"),
    ("mov", "video/quicktime"),
];

fn kind_of(content_type: &str) -> Option<&'static str> {
    CONTENT_TYPES
        .iter()
        .find(|(ct, _)| ct.eq_ignore_ascii_case(content_type))
        .map(|(_, k)| *k)
}

fn type_for_extension(name: &str) -> Option<&'static str> {
    let ext = Path::new(name).extension()?.to_str()?.to_ascii_lowercase();
    EXTENSIONS.iter().find(|(e, _)| *e == ext).map(|(_, ct)| *ct)
}

/// Checks the file's first bytes really look like the claimed type (a renamed .exe fails).
fn looks_like(content_type: &str, h: &[u8]) -> bool {
    let at = |offset: usize, magic: &[u8]| h.len() >= offset + magic.len() && &h[offset..offset + magic.len()] == magic;
    let id3 = at(0, b"ID3");
    let iso_box = || {
        [&b"ftyp"[..], b"moov", b"mdat", b"wide", b"free", b"skip"]
            .iter()
            .any(|m| at(4, m))
    };
    match content_type {
        "image/jpeg" => at(0, &[0xFF, 0xD8, 0xFF]),
        "image/png" => at(0, &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        "image/gif" => at(0, b"GIF87a") || at(0, b"GIF89a"),
        "image/webp" => at(0, b"RIFF") && at(8, b"WEBP"),
        "image/avif" => at(4, b"ftyp"),
        "audio/mpeg" => id3 || (h.len() >= 2 && h[0] == 0xFF && h[1] & 0xE0 == 0xE0),
        "audio/aac" => id3 || (h.len() >= 2 && h[0] == 0xFF && h[1] & 0xF6 == 0xF0),
        "audio/flac" => id3 || at(0, b"fLaC"),
        "audio/ogg" | "video/ogg" => at(0, b"OggS"),
        "audio/wav" => at(0, b"RIFF") && at(8, b"WAVE"),
        "audio/mp4" | "video/mp4" | "video/quicktime" => iso_box(),
        "audio/webm" | "video/webm" => at(0, &[0x1A, 0x45, 0xDF, 0xA3]),
        _ => false,
    }
}

fn clean_name(name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).collect();
    let name = name.trim();
    let name = if name.is_empty() { "media" } else { name };
    name.chars().take(MEDIA_NAME_MAX).collect()
}

/// Validates and stores media bytes under a fresh id; returns the reference put into the set.
fn store_media(d: &Dirs, bytes: &[u8], content_type: &str, name: &str) -> Res<Value> {
    if bytes.is_empty() {
        return Err("err.mediaEmpty".into());
    }
    if bytes.len() as u64 > MAX_MEDIA_BYTES {
        return Err("err.mediaTooLarge".into());
    }
    let content_type = content_type.to_ascii_lowercase();
    let kind = kind_of(&content_type).ok_or("err.mediaFormat")?;
    if !looks_like(&content_type, &bytes[..bytes.len().min(32)]) {
        return Err("err.mediaFormat".into());
    }
    let id = Uuid::new_v4().hyphenated().to_string();
    let name = clean_name(name);
    write_atomic(&d.media.join(&id), bytes)?;
    let meta = json!({ "kind": kind, "name": name, "contentType": content_type, "size": bytes.len() });
    write_atomic(&d.media.join(format!("{id}.json")), &serde_json::to_vec(&meta).map_err(err)?)?;
    Ok(json!({ "id": id, "kind": kind, "name": name }))
}

fn media_meta(d: &Dirs, id: &str) -> Option<Value> {
    let text = fs::read_to_string(d.media.join(format!("{id}.json"))).ok()?;
    serde_json::from_str(&text).ok()
}

/// Deletes media no set refers to any more (older than a day, so a file just added to a set
/// that has not been saved yet survives).
fn clean_up_media(d: &Dirs) {
    let mut used = HashSet::new();
    if let Ok(entries) = fs::read_dir(&d.sets) {
        for entry in entries.flatten() {
            if let Ok(set) = fs::read_to_string(entry.path()).map(|t| serde_json::from_str::<Value>(&t)) {
                if let Ok(set) = set {
                    used.extend(media_ids(&set));
                }
            }
        }
    }
    let Ok(entries) = fs::read_dir(&d.media) else { return };
    let day_ago = SystemTime::now() - Duration::from_secs(24 * 3600);
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let id = name.trim_end_matches(".json").trim_end_matches(".tmp");
        if used.contains(id) {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| t < day_ago)
            .unwrap_or(false);
        if old {
            let _ = fs::remove_file(entry.path());
        }
    }
}

// ---------- commands ----------

#[tauri::command]
fn list_sets(app: AppHandle) -> Res<Vec<Value>> {
    let d = dirs(&app)?;
    let mut out = Vec::new();
    for entry in fs::read_dir(&d.sets).map_err(err)?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else { continue };
        let Ok(set) = serde_json::from_str::<Value>(&text) else { continue };
        if set.get("id").and_then(Value::as_str).is_some() {
            out.push(set);
        }
    }
    Ok(out)
}

#[tauri::command]
fn get_set(app: AppHandle, id: String) -> Res<Value> {
    read_set(&dirs(&app)?, &id)
}

#[tauri::command]
fn save_set(app: AppHandle, set: Value) -> Res<Value> {
    store_set(&dirs(&app)?, set)
}

#[tauri::command]
fn delete_set(app: AppHandle, id: String) -> Res<()> {
    let d = dirs(&app)?;
    let id = canonical_id(&id)?;
    fs::remove_file(d.sets.join(format!("{id}.json"))).map_err(err)
}

/// Decodes `encodeURIComponent` output (header values must be ASCII).
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The served type of a file, like GameShow's MediaFormats.Resolve: the browser's type when it
/// is allowed (with its aliases), otherwise the file extension.
fn resolve_type(content_type: &str, name: &str) -> Option<&'static str> {
    let ct = content_type.trim().to_ascii_lowercase();
    let alias = match ct.as_str() {
        "audio/mp3" => "audio/mpeg",
        "audio/x-wav" | "audio/wave" => "audio/wav",
        "audio/x-m4a" => "audio/mp4",
        other => other,
    };
    CONTENT_TYPES
        .iter()
        .find(|(known, _)| *known == alias)
        .map(|(known, _)| *known)
        .or_else(|| type_for_extension(name))
}

/// Stores a file the editor's media picker sends: raw bytes in the body, the name and type in
/// the `x-file-name` (URI-encoded) and `x-content-type` headers.
#[tauri::command]
fn add_media(app: AppHandle, request: tauri::ipc::Request<'_>) -> Res<Value> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("err.mediaEmpty".into());
    };
    let header = |key: &str| request.headers().get(key).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let name = percent_decode(&header("x-file-name"));
    let content_type = resolve_type(&header("x-content-type"), &name).ok_or("err.mediaFormat")?;
    store_media(&dirs(&app)?, bytes, content_type, &name)
}

fn file_slug(title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars() {
        if c.is_alphanumeric() {
            slug.extend(c.to_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() { "zestaw".into() } else { slug.chars().take(60).collect() }
}

/// Saves the set as `<title>.gameshow.zip`: set.json + manifest.json + media/<id>.
#[tauri::command]
async fn export_zip(app: AppHandle, id: String) -> Res<Option<String>> {
    let d = dirs(&app)?;
    let set = read_set(&d, &id)?;
    let title = set.get("title").and_then(Value::as_str).unwrap_or("").to_string();
    let Some(dest) = app
        .dialog()
        .file()
        .set_file_name(format!("{}.gameshow.zip", file_slug(&title)))
        .add_filter("GameShow", &["zip"])
        .blocking_save_file()
    else {
        return Ok(None);
    };
    let dest = dest.into_path().map_err(err)?;
    write_export(&d, &set, &dest)?;
    Ok(Some(dest.to_string_lossy().to_string()))
}

fn write_export(d: &Dirs, set: &Value, dest: &Path) -> Res<()> {
    let mut file = Map::new();
    file.insert("format".into(), json!(SET_FORMAT));
    file.insert("version".into(), json!(SET_VERSION));
    for key in SET_FIELDS {
        file.insert(key.into(), set.get(key).cloned().unwrap_or(Value::Null));
    }
    let mut file = Value::Object(file);

    // Only media that still exist go into the zip; a reference to a missing file is dropped.
    let mut manifest = Vec::new();
    let mut present = HashSet::new();
    for mid in media_ids(set) {
        if let Some(meta) = media_meta(d, &mid) {
            if d.media.join(&mid).is_file() {
                manifest.push(json!({
                    "id": mid, "kind": meta.get("kind"), "name": meta.get("name"),
                    "contentType": meta.get("contentType"), "size": meta.get("size"),
                }));
                present.insert(mid);
            }
        }
    }
    each_media_ref(&mut file, &mut |v| {
        let keep = v.get("id").and_then(Value::as_str).and_then(|s| canonical_id(s).ok()).is_some_and(|i| present.contains(&i));
        if !keep {
            *v = Value::Null;
        }
    });

    let tmp = dest.with_extension("zip.tmp");
    {
        let out = fs::File::create(&tmp).map_err(err)?;
        let mut zip = zip::ZipWriter::new(out);
        let deflate = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .large_file(false);
        zip.start_file("set.json", deflate).map_err(err)?;
        zip.write_all(&serde_json::to_vec_pretty(&file).map_err(err)?).map_err(err)?;
        zip.start_file("manifest.json", deflate).map_err(err)?;
        zip.write_all(&serde_json::to_vec_pretty(&json!({ "media": manifest })).map_err(err)?).map_err(err)?;
        for mid in &present {
            zip.start_file(format!("media/{mid}"), stored).map_err(err)?;
            let mut src = fs::File::open(d.media.join(mid)).map_err(err)?;
            std::io::copy(&mut src, &mut zip).map_err(err)?;
        }
        zip.finish().map_err(err)?;
    }
    fs::rename(&tmp, dest).map_err(err)
}

/// Reads at most `limit` bytes of a reader; errors when there is more (counts real bytes, so a
/// zip entry that lies about its size can't blow up memory).
fn read_capped<R: Read>(reader: R, limit: u64, too_big: &str) -> Res<Vec<u8>> {
    let mut buf = Vec::new();
    reader.take(limit + 1).read_to_end(&mut buf).map_err(err)?;
    if buf.len() as u64 > limit {
        return Err(too_big.into());
    }
    Ok(buf)
}

fn parse_set_file(bytes: &[u8]) -> Res<Value> {
    let data: Value = serde_json::from_slice(bytes).map_err(|_| "err.badFile")?;
    if data.get("format").and_then(Value::as_str) != Some(SET_FORMAT) {
        return Err("err.badFile".into());
    }
    Ok(data)
}

/// A new stored set from an imported file: only known fields, a fresh id.
fn new_set_from(file: &Value) -> Value {
    let mut set = Map::new();
    set.insert("id".into(), json!(Uuid::new_v4().hyphenated().to_string()));
    for key in SET_FIELDS {
        if let Some(v) = file.get(key) {
            set.insert(key.into(), v.clone());
        }
    }
    Value::Object(set)
}

/// Opens a `.gameshow.zip` (or a plain GameShow `.json` export) as a new set.
#[tauri::command]
async fn import_file(app: AppHandle) -> Res<Option<Value>> {
    let Some(picked) = app
        .dialog()
        .file()
        .add_filter("GameShow", &["zip", "json"])
        .blocking_pick_file()
    else {
        return Ok(None);
    };
    let path = picked.into_path().map_err(err)?;
    let d = dirs(&app)?;
    let is_json = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("json"));

    let (mut set, dropped) = if is_json {
        // A plain .json export: its media live on a GameShow server, so references are dropped.
        let bytes = read_capped(fs::File::open(&path).map_err(err)?, MAX_JSON_BYTES, "err.fileTooLarge")?;
        let mut set = new_set_from(&parse_set_file(&bytes)?);
        let mut dropped = 0;
        each_media_ref(&mut set, &mut |v| {
            if !v.is_null() {
                dropped += 1;
                *v = Value::Null;
            }
        });
        (set, dropped)
    } else {
        import_zip(&d, &path)?
    };

    // A lone second file moves to the first slot (same rule as GameShow).
    fix_media_slots(&mut set);
    let set = store_set(&d, set)?;
    Ok(Some(json!({ "id": set.get("id"), "title": set.get("title"), "droppedMedia": dropped })))
}

fn import_zip(d: &Dirs, path: &Path) -> Res<(Value, usize)> {
    let file = fs::File::open(path).map_err(err)?;
    let mut zip = zip::ZipArchive::new(file).map_err(|_| "err.badFile")?;
    if zip.len() > MAX_ZIP_ENTRIES {
        return Err("err.zipTooManyFiles".into());
    }
    let set_bytes = read_capped(zip.by_name("set.json").map_err(|_| "err.badFile")?, MAX_JSON_BYTES, "err.fileTooLarge")?;
    let mut set = new_set_from(&parse_set_file(&set_bytes)?);

    let manifest: HashMap<String, Value> = match zip.by_name("manifest.json") {
        Ok(entry) => {
            let bytes = read_capped(entry, MAX_JSON_BYTES, "err.fileTooLarge")?;
            let manifest: Value = serde_json::from_slice(&bytes).map_err(|_| "err.badFile")?;
            manifest
                .get("media")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|m| {
                    let id = canonical_id(m.get("id")?.as_str()?).ok()?;
                    Some((id, m.clone()))
                })
                .collect()
        }
        Err(_) => HashMap::new(),
    };

    // Copy each referenced file once, under a new id. Entry names are never used as paths.
    let mut total = 0u64;
    let mut mapped: HashMap<String, Option<Value>> = HashMap::new();
    for old in media_ids(&set) {
        let new_ref = (|| -> Res<Value> {
            let entry = zip.by_name(&format!("media/{old}")).map_err(|_| "err.missing")?;
            if entry.size() > MAX_MEDIA_BYTES {
                return Err("err.mediaTooLarge".into());
            }
            let bytes = read_capped(entry, MAX_MEDIA_BYTES, "err.mediaTooLarge")?;
            total += bytes.len() as u64;
            if total > MAX_ZIP_TOTAL {
                return Err("err.zipTooLarge".into());
            }
            let meta = manifest.get(&old);
            let name = meta.and_then(|m| m.get("name")).and_then(Value::as_str).unwrap_or("media");
            let content_type = meta
                .and_then(|m| m.get("contentType"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| type_for_extension(name).map(str::to_string))
                .ok_or("err.mediaFormat")?;
            store_media(d, &bytes, &content_type, name)
        })();
        match new_ref {
            Ok(r) => {
                mapped.insert(old, Some(r));
            }
            Err(e) if e == "err.zipTooLarge" => return Err(e),
            Err(_) => {
                mapped.insert(old, None);
            }
        }
    }

    let mut dropped = 0;
    each_media_ref(&mut set, &mut |v| {
        if v.is_null() {
            return;
        }
        let new_ref = v
            .get("id")
            .and_then(Value::as_str)
            .and_then(|s| canonical_id(s).ok())
            .and_then(|id| mapped.get(&id).cloned().flatten());
        match new_ref {
            Some(r) => *v = r,
            None => {
                dropped += 1;
                *v = Value::Null;
            }
        }
    });
    Ok((set, dropped))
}

fn fix_media_slots(value: &mut Value) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(fix_media_slots),
        Value::Object(map) => {
            let first_empty = map.get("media").is_none_or(Value::is_null);
            let second = map.get("secondMedia").cloned().unwrap_or(Value::Null);
            if map.contains_key("secondMedia") && first_empty && !second.is_null() {
                map.insert("media".into(), second);
                map.insert("secondMedia".into(), Value::Null);
            }
            for v in map.values_mut() {
                fix_media_slots(v);
            }
        }
        _ => {}
    }
}

// ---------- media:// protocol (previews in the editor) ----------

fn serve_media(app: &AppHandle, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let fail = |status: StatusCode| Response::builder().status(status).body(Vec::new()).unwrap();
    let Ok(d) = dirs(app) else { return fail(StatusCode::INTERNAL_SERVER_ERROR) };
    let Ok(id) = canonical_id(request.uri().path().trim_matches('/')) else { return fail(StatusCode::NOT_FOUND) };
    let content_type = media_meta(&d, &id)
        .and_then(|m| m.get("contentType").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| "application/octet-stream".into());
    let Ok(mut file) = fs::File::open(d.media.join(&id)) else { return fail(StatusCode::NOT_FOUND) };
    let Ok(size) = file.metadata().map(|m| m.len()) else { return fail(StatusCode::NOT_FOUND) };

    let range = request
        .headers()
        .get("range")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| {
            let (start, end) = v.split_once('-')?;
            let start: u64 = start.trim().parse().ok()?;
            let end: u64 = end.trim().parse().unwrap_or(size.saturating_sub(1));
            Some((start, end.min(size.saturating_sub(1))))
        });

    let builder = Response::builder()
        .header("Content-Type", content_type)
        .header("Accept-Ranges", "bytes")
        .header("X-Content-Type-Options", "nosniff");
    match range {
        Some((start, end)) if start < size && start <= end => {
            let end = end.min(start + MAX_RANGE_CHUNK - 1);
            let mut buf = vec![0u8; (end - start + 1) as usize];
            if file.seek(SeekFrom::Start(start)).is_err() || file.read_exact(&mut buf).is_err() {
                return fail(StatusCode::INTERNAL_SERVER_ERROR);
            }
            builder
                .status(StatusCode::PARTIAL_CONTENT)
                .header("Content-Range", format!("bytes {start}-{end}/{size}"))
                .body(buf)
                .unwrap()
        }
        Some(_) => Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header("Content-Range", format!("bytes */{size}"))
            .body(Vec::new())
            .unwrap(),
        None => {
            let mut buf = Vec::with_capacity(size as usize);
            if file.read_to_end(&mut buf).is_err() {
                return fail(StatusCode::INTERNAL_SERVER_ERROR);
            }
            builder.status(StatusCode::OK).body(buf).unwrap()
        }
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .register_asynchronous_uri_scheme_protocol("media", |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            std::thread::spawn(move || responder.respond(serve_media(&app, &request)));
        })
        .setup(|app| {
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                if let Ok(d) = dirs(&handle) {
                    clean_up_media(&d);
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_sets,
            get_set,
            save_set,
            delete_set,
            add_media,
            export_zip,
            import_file
        ])
        .run(tauri::generate_context!())
        .expect("error while running Set Creator");
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: [u8; 16] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];

    fn temp_dirs() -> (PathBuf, Dirs) {
        let base = std::env::temp_dir().join(format!("setcreator-test-{}", Uuid::new_v4()));
        let d = Dirs { sets: base.join("sets"), media: base.join("media") };
        fs::create_dir_all(&d.sets).unwrap();
        fs::create_dir_all(&d.media).unwrap();
        (base, d)
    }

    fn question(media: Value) -> Value {
        json!({ "text": "Q", "type": "Open", "answer": "A", "options": [], "correctOption": null, "media": media, "secondMedia": null, "time": null })
    }

    fn set_with(media: Value) -> Value {
        json!({
            "format": SET_FORMAT, "version": 2, "title": "Test", "mode": "Teams", "rowPoints": [10],
            "categories": [{ "name": "C", "questions": [question(media)], "time": null }],
            "stage2Cards": [], "lineBonuses": [], "stage3Questions": [], "stage4Questions": [],
            "timers": { "stageSeconds": [30, 30, 30, 30], "rowSeconds": [], "bidSeconds": 15, "soloSeconds": 10 },
            "soloQuestions": []
        })
    }

    fn make_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    fn media_count(d: &Dirs) -> usize {
        fs::read_dir(&d.media).unwrap().count()
    }

    #[test]
    fn export_then_import_round_trips_media() {
        let (base, d) = temp_dirs();
        let media = store_media(&d, &PNG, "image/png", "pic.png").unwrap();
        let mut set = new_set_from(&set_with(media.clone()));
        set = store_set(&d, set).unwrap();
        let zip_path = base.join("out.gameshow.zip");
        write_export(&d, &set, &zip_path).unwrap();

        let (imported, dropped) = import_zip(&d, &zip_path).unwrap();
        assert_eq!(dropped, 0);
        let new_media = &imported["categories"][0]["questions"][0]["media"];
        assert_ne!(new_media["id"], media["id"]);
        assert_eq!(new_media["kind"], "Image");
        assert_eq!(new_media["name"], "pic.png");
        let new_id = new_media["id"].as_str().unwrap();
        assert_eq!(fs::read(d.media.join(new_id)).unwrap(), PNG);
        assert_eq!(imported["title"], "Test");
        assert!(imported.get("format").is_none());

        // The exported set.json is GameShow's format.
        let mut zip = zip::ZipArchive::new(fs::File::open(&zip_path).unwrap()).unwrap();
        let file: Value = serde_json::from_reader(zip.by_name("set.json").unwrap()).unwrap();
        assert_eq!(file["format"], "gameshow-set");
        assert_eq!(file["version"], 2);
        assert!(file.get("id").is_none() && file.get("updatedAt").is_none());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn path_traversal_entries_are_never_written() {
        let (base, d) = temp_dirs();
        let id = Uuid::new_v4().to_string();
        let set = set_with(json!({ "id": "../../evil", "kind": "Image", "name": "x.png" }));
        let zip_path = base.join("evil.zip");
        make_zip(&zip_path, &[
            ("set.json", &serde_json::to_vec(&set).unwrap()),
            ("../evil.png", &PNG),
            ("media/../../evil2.png", &PNG),
            (&format!("media/{id}"), &PNG),
        ]);
        let (imported, dropped) = import_zip(&d, &zip_path).unwrap();
        assert_eq!(dropped, 1);
        assert!(imported["categories"][0]["questions"][0]["media"].is_null());
        assert_eq!(media_count(&d), 0);
        assert!(!base.join("evil.png").exists() && !base.parent().unwrap().join("evil2.png").exists());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn oversized_and_fake_media_are_dropped() {
        let (base, d) = temp_dirs();
        let big = Uuid::new_v4().to_string();
        let fake = Uuid::new_v4().to_string();
        let mut set = set_with(json!({ "id": big, "kind": "Image", "name": "big.png" }));
        set["categories"][0]["questions"][0]["secondMedia"] = json!({ "id": fake, "kind": "Image", "name": "fake.png" });
        let mut huge = PNG.to_vec();
        huge.resize((MAX_MEDIA_BYTES + 1) as usize, 0);
        let manifest = json!({ "media": [
            { "id": big, "kind": "Image", "name": "big.png", "contentType": "image/png" },
            { "id": fake, "kind": "Image", "name": "fake.png", "contentType": "image/png" }
        ]});
        let zip_path = base.join("bad.zip");
        make_zip(&zip_path, &[
            ("set.json", &serde_json::to_vec(&set).unwrap()),
            ("manifest.json", &serde_json::to_vec(&manifest).unwrap()),
            (&format!("media/{big}"), &huge),
            (&format!("media/{fake}"), b"MZ\x90\x00 this is an exe"),
        ]);
        let (_, dropped) = import_zip(&d, &zip_path).unwrap();
        assert_eq!(dropped, 2);
        assert_eq!(media_count(&d), 0);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn zip_bomb_set_json_is_rejected() {
        let (base, d) = temp_dirs();
        let zip_path = base.join("bomb.zip");
        let mut bomb = br#"{"format":"gameshow-set","title":""#.to_vec();
        bomb.resize(3 * 1024 * 1024, b'a');
        make_zip(&zip_path, &[("set.json", &bomb)]);
        assert!(fs::metadata(&zip_path).unwrap().len() < 100 * 1024);
        assert_eq!(import_zip(&d, &zip_path).unwrap_err(), "err.fileTooLarge");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn too_many_entries_and_wrong_format_are_rejected() {
        let (base, d) = temp_dirs();
        let many = base.join("many.zip");
        let names: Vec<String> = (0..=MAX_ZIP_ENTRIES).map(|i| format!("f{i}")).collect();
        let entries: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b"x"[..])).collect();
        make_zip(&many, &entries);
        assert_eq!(import_zip(&d, &many).unwrap_err(), "err.zipTooManyFiles");

        let wrong = base.join("wrong.zip");
        make_zip(&wrong, &[("set.json", br#"{"format":"something-else"}"#)]);
        assert_eq!(import_zip(&d, &wrong).unwrap_err(), "err.badFile");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn media_names_and_types_resolve_like_gameshow() {
        assert_eq!(percent_decode("Zdj%C4%99cie%201.png"), "Zdjęcie 1.png");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(resolve_type("audio/x-m4a", "a.bin"), Some("audio/mp4"));
        assert_eq!(resolve_type("", "clip.MOV"), Some("video/quicktime"));
        assert_eq!(resolve_type("image/svg+xml", "logo.svg"), None);
        assert_eq!(resolve_type("application/octet-stream", "x.exe"), None);
    }

    #[test]
    fn lone_second_media_moves_to_first_slot() {
        let mut q = json!({ "media": null, "secondMedia": { "id": "x" } });
        fix_media_slots(&mut q);
        assert_eq!(q["media"]["id"], "x");
        assert!(q["secondMedia"].is_null());
    }
}
