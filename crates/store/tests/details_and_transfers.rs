//! What the terminal UI needs from the store beyond the command line: each app's license and each
//! version's permissions, and downloads that report their bytes and can be cancelled. Repositories
//! are made by the testkit and read from disk, so no test needs the network.

use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use uxdroid_store::fetch::Fetch;
use uxdroid_store::{Device, Error, Permissions, Refresh, Store};
use uxdroid_testkit::fdroid::{self, App, Version};
use uxdroid_testkit::{Apk, Key, app_manifest, dex};

const URL: &str = "http://127.0.0.1/repo";
const FIRST: u64 = 1_793_000_000_000;
const SECOND: u64 = 1_793_100_000_000;

fn scratch(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("details-{name}"));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// Serves the files of a directory for `URL`, in chunks of 1000 bytes, and records what is asked.
struct Files {
    root: PathBuf,
    asked: Rc<RefCell<Vec<String>>>,
}

impl Fetch for Files {
    fn copy(&self, url: &str, limit: u64, sink: &mut dyn Write) -> Result<u64, Error> {
        self.asked.borrow_mut().push(url.to_string());
        let name = url
            .strip_prefix(URL)
            .ok_or_else(|| Error::new(format!("cannot fetch {url}")))?;
        let bytes = std::fs::read(self.root.join(name.trim_start_matches('/')))
            .map_err(|_| Error::new(format!("cannot fetch {url}: the server answered 404")))?;
        if bytes.len() as u64 > limit {
            return Err(Error::new(format!(
                "{url} is larger than the {limit} bytes allowed"
            )));
        }
        for chunk in bytes.chunks(1000) {
            sink.write_all(chunk)
                .map_err(|error| Error::new(format!("cannot store {url}: {error}")))?;
        }
        Ok(bytes.len() as u64)
    }
}

/// Bytes that do not compress, so the APK stays larger than they are.
fn noise(length: usize) -> Vec<u8> {
    let mut state: u32 = 12345;
    (0..length)
        .map(|_| {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (state >> 16) as u8
        })
        .collect()
}

fn apk(package: &'static str, code: u32) -> Vec<u8> {
    Apk::new()
        .manifest(&app_manifest(package, code, "1.0"))
        .file("classes.dex", &dex(1, 1))
        .file("assets/noise", &noise(5000))
        .signed_bytes(&Key::test())
}

fn notes(code: u64) -> App {
    let package = "com.example.notes";
    App::new(package, "Notes", "Write markdown", FIRST).version(Version::new(
        package,
        code,
        "1.0",
        apk(package, code as u32),
        &Key::test(),
    ))
}

/// A store in `dir/data` whose only enabled source is the repository in `dir/repo`.
fn store(dir: &Path) -> (Store, Rc<RefCell<Vec<String>>>) {
    let asked = Rc::new(RefCell::new(Vec::new()));
    let fetch = Files {
        root: dir.join("repo"),
        asked: asked.clone(),
    };
    let mut store = Store::open_with(&dir.join("data"), Box::new(fetch)).unwrap();
    store
        .change(|config| {
            config.set_enabled("fdroid", false)?;
            config
                .add("test", URL, &Key::test().fingerprint())
                .map(|_| ())
        })
        .unwrap();
    (store, asked)
}

fn internet(package: &str, code: u64) -> Permissions {
    Permissions::from([(
        ("test".to_string(), package.to_string(), code),
        vec!["android.permission.INTERNET".to_string()],
    )])
}

#[test]
fn the_index_gives_each_app_its_license_and_each_release_its_permissions() {
    let dir = scratch("license");
    fdroid::write_repo(&dir.join("repo"), FIRST, &[notes(1)], &[], &Key::test());
    let (store, _) = store(&dir);
    assert_eq!(
        store.permissions(),
        Permissions::new(),
        "nothing before a read"
    );
    let (apps, _) = store.apps(None, Refresh::IfStale).unwrap();
    assert_eq!(apps[0].license.as_deref(), Some("Apache-2.0"));
    assert_eq!(store.permissions(), internet("com.example.notes", 1));
    // The index the commands read does not hold them.
    let index = std::fs::read_to_string(dir.join("data/store/sources/test/index.json")).unwrap();
    assert!(!index.contains("android.permission"), "{index}");
}

#[test]
fn a_cache_kept_before_licenses_and_permissions_is_fetched_whole_on_its_next_refresh() {
    let dir = scratch("old-cache");
    let repo = dir.join("repo");
    let first = fdroid::write_repo(&repo, FIRST, &[notes(1)], &[], &Key::test());
    let (store, asked) = store(&dir);
    store.apps(None, Refresh::IfStale).unwrap();
    // A cache as uxdroid kept it before: no license, no permissions, no mark of the fields kept.
    let cache = dir.join("data/store/sources/test");
    let mut index: serde_json::Value =
        serde_json::from_slice(&std::fs::read(cache.join("index.json")).unwrap()).unwrap();
    let package = &mut index["packages"]["com.example.notes"];
    package["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("license");
    std::fs::write(
        cache.join("index.json"),
        serde_json::to_vec(&index).unwrap(),
    )
    .unwrap();
    std::fs::remove_file(cache.join("permissions.json")).unwrap();
    let mut state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(cache.join("state.json")).unwrap()).unwrap();
    state.as_object_mut().unwrap().remove("fields");
    std::fs::write(
        cache.join("state.json"),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
    let (apps, _) = store.apps(None, Refresh::IfStale).unwrap();
    assert_eq!(apps[0].license, None, "a fresh old cache is still read");
    assert_eq!(store.permissions(), Permissions::new());

    // The repository moves on and offers a diff; the old cache needs the whole index instead.
    let old: serde_json::Value = serde_json::from_slice(&first).unwrap();
    let new = fdroid::index(SECOND, &[notes(2)]);
    let patch = serde_json::to_vec(&fdroid::diff(&old, &new)).unwrap();
    fdroid::write_repo(&repo, SECOND, &[notes(2)], &[(FIRST, patch)], &Key::test());
    asked.borrow_mut().clear();
    let (apps, _) = store.apps(None, Refresh::Always).unwrap();
    assert_eq!(
        asked.borrow().as_slice(),
        [format!("{URL}/entry.jar"), format!("{URL}/index-v2.json")]
    );
    assert_eq!(apps[0].license.as_deref(), Some("Apache-2.0"));
    assert_eq!(store.permissions(), internet("com.example.notes", 2));

    // A cache that has them takes diffs as before.
    let older: serde_json::Value = new;
    let newest = fdroid::index(SECOND + 1, &[notes(3)]);
    let patch = serde_json::to_vec(&fdroid::diff(&older, &newest)).unwrap();
    fdroid::write_repo(
        &repo,
        SECOND + 1,
        &[notes(3)],
        &[(SECOND, patch)],
        &Key::test(),
    );
    asked.borrow_mut().clear();
    let (apps, _) = store.apps(None, Refresh::Always).unwrap();
    assert_eq!(
        asked.borrow().as_slice(),
        [
            format!("{URL}/entry.jar"),
            format!("{URL}/diff/{SECOND}.json")
        ]
    );
    assert_eq!(apps[0].releases[0].version_code, 3);
    assert_eq!(apps[0].license.as_deref(), Some("Apache-2.0"));
    assert_eq!(
        store.permissions(),
        internet("com.example.notes", 3),
        "the diff applies to the permissions too"
    );
}

#[test]
fn a_download_reports_its_bytes_as_they_arrive_and_can_be_cancelled() {
    let dir = scratch("transfer");
    fdroid::write_repo(&dir.join("repo"), FIRST, &[notes(1)], &[], &Key::test());
    let (mut store, _) = store(&dir);
    let (apps, _) = store.apps(None, Refresh::IfStale).unwrap();
    let app = &apps[0];
    let release = app.newest(&Device::host()).unwrap();
    assert!(release.size > 2000, "the APK spans several chunks");

    let seen = Rc::new(RefCell::new(Vec::new()));
    let record = seen.clone();
    store.on_transfer(Box::new(move |done, total| {
        record.borrow_mut().push((done, total));
        true
    }));
    let download = store.download(app, release).unwrap();
    let seen = seen.borrow().clone();
    assert!(seen.len() >= 2, "{seen:?}");
    assert!(
        seen.windows(2).all(|pair| pair[0].0 < pair[1].0),
        "{seen:?}"
    );
    assert!(seen.iter().all(|(_, total)| *total == release.size));
    assert_eq!(seen.last(), Some(&(release.size, release.size)));
    drop(download);

    store.on_transfer(Box::new(|done, _| done < 1000));
    let error = store.download(app, release).err().unwrap();
    assert_eq!(
        error.0,
        "the download of com.example.notes from test was cancelled"
    );
    let left: Vec<_> = std::fs::read_dir(dir.join("data/store/downloads"))
        .unwrap()
        .collect();
    assert!(left.is_empty(), "a cancelled download leaves no file");
}

#[test]
fn a_missing_or_corrupt_permissions_file_is_rebuilt_from_the_whole_index() {
    let dir = scratch("repair");
    let repo = dir.join("repo");
    let first = fdroid::write_repo(&repo, FIRST, &[notes(1)], &[], &Key::test());
    let (store, asked) = store(&dir);
    store.apps(None, Refresh::IfStale).unwrap();
    let permissions = dir.join("data/store/sources/test/permissions.json");
    let entry = format!("{URL}/entry.jar");
    let whole = format!("{URL}/index-v2.json");
    for damage in ["missing", "corrupt"] {
        match damage {
            "missing" => std::fs::remove_file(&permissions).unwrap(),
            _ => std::fs::write(&permissions, b"\x00\xff not json").unwrap(),
        }
        asked.borrow_mut().clear();
        // The repository has not changed: only the whole index can rebuild the file.
        store.apps(None, Refresh::Always).unwrap();
        assert_eq!(
            asked.borrow().as_slice(),
            [entry.clone(), whole.clone()],
            "{damage}"
        );
        assert_eq!(
            store.permissions(),
            internet("com.example.notes", 1),
            "{damage}"
        );
    }
    asked.borrow_mut().clear();
    store.apps(None, Refresh::Always).unwrap();
    assert_eq!(
        asked.borrow().as_slice(),
        std::slice::from_ref(&entry),
        "repaired: nothing more"
    );

    // A diff is never merged onto a missing file: the whole index is read instead.
    std::fs::remove_file(&permissions).unwrap();
    let old: serde_json::Value = serde_json::from_slice(&first).unwrap();
    let new = fdroid::index(SECOND, &[notes(2)]);
    let patch = serde_json::to_vec(&fdroid::diff(&old, &new)).unwrap();
    fdroid::write_repo(&repo, SECOND, &[notes(2)], &[(FIRST, patch)], &Key::test());
    asked.borrow_mut().clear();
    store.apps(None, Refresh::Always).unwrap();
    assert_eq!(asked.borrow().as_slice(), [entry, whole]);
    assert_eq!(store.permissions(), internet("com.example.notes", 2));
}
