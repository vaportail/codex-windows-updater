//! MSIX extraction + version directory management.
//!
//! MSIX is a ZIP archive. The Electron app we care about lives under the
//! `app/` prefix; everything else (AppxManifest.xml, AppxBlockMap.xml,
//! AppxSignature.p7x, Assets/, resources.pri, ...) is Store packaging
//! metadata we don't need to run Codex standalone.
//!
//! Layout produced:
//!   <install_root>/versions/<version>/ChatGPT.exe  (or Codex.exe on older builds)
//!   <install_root>/versions/<version>/resources/app.asar
//!   ...
//!
//! Extraction writes to `<version>.partial/` first and renames on success,
//! so a crash mid-extract never leaves a half-populated directory that
//! looks valid to the proxy launcher.

use anyhow::{bail, Context, Result};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const APP_PREFIX: &str = "app/";

/// Extract the `app/` subtree from `msix_path` into
/// `<install_root>/versions/<version>/`. Any pre-existing directory at that
/// path is removed first. Progress fires per-entry with (done_entries, total_entries).
pub fn extract_app(
    msix_path: &Path,
    install_root: &Path,
    version: &str,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<PathBuf> {
    let versions_dir = install_root.join("versions");
    fs::create_dir_all(&versions_dir)
        .with_context(|| format!("creating {}", versions_dir.display()))?;

    let final_dir = versions_dir.join(version);
    let partial_dir = versions_dir.join(format!("{version}.partial"));

    if partial_dir.exists() {
        fs::remove_dir_all(&partial_dir)
            .with_context(|| format!("clearing stale {}", partial_dir.display()))?;
    }
    fs::create_dir_all(&partial_dir)?;

    let file =
        fs::File::open(msix_path).with_context(|| format!("opening {}", msix_path.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("reading {} as zip", msix_path.display()))?;

    // First pass: count app/ entries so progress has a total.
    let mut total_app_entries: u64 = 0;
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        if entry.name().starts_with(APP_PREFIX) && !entry.is_dir() {
            total_app_entries += 1;
        }
    }
    if total_app_entries == 0 {
        bail!(
            "MSIX contains no entries under '{}' — wrong package? {}",
            APP_PREFIX,
            msix_path.display()
        );
    }

    let mut done: u64 = 0;
    progress(done, Some(total_app_entries));

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name().to_string();
        if !name.starts_with(APP_PREFIX) {
            continue;
        }
        let rel = &name[APP_PREFIX.len()..];
        if rel.is_empty() {
            continue;
        }
        let decoded_rel = decode_package_path(rel)?;
        let out_path = safe_join(&partial_dir, &decoded_rel)?;

        if entry.is_dir() {
            fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = fs::File::create(&out_path)
            .with_context(|| format!("creating {}", out_path.display()))?;
        io::copy(&mut entry, &mut out)?;

        done += 1;
        progress(done, Some(total_app_entries));
    }

    disable_native_updater(&partial_dir)?;

    if final_dir.exists() {
        fs::remove_dir_all(&final_dir)
            .with_context(|| format!("removing old {}", final_dir.display()))?;
    }
    rename_with_retry(&partial_dir, &final_dir).with_context(|| {
        format!(
            "rename {} -> {}",
            partial_dir.display(),
            final_dir.display()
        )
    })?;

    if crate::proxy::app_exe_in(&final_dir).is_none() {
        bail!(
            "extracted tree has no ChatGPT.exe/Codex.exe under {} — MSIX layout changed?",
            final_dir.display()
        );
    }

    Ok(final_dir)
}

/// The native Store updater can throw "The process has no package identity"
/// during bootstrap in an unpackaged install. Codex handles an absent addon,
/// and this launcher already manages updates. Preserve its bytes under a name
/// that Node's native-addon loader will not find.
pub fn disable_native_updater(app_dir: &Path) -> Result<()> {
    let source = app_dir.join("resources/native/windows-updater.node");
    if !source.try_exists()? {
        return Ok(());
    }
    let destination = source.with_file_name("windows-updater.broken");
    // Do not overwrite an existing backup. A normal second launch is a no-op
    // because the .node file is already absent.
    if destination.try_exists()? {
        if !source.try_exists()? {
            return Ok(());
        }
        bail!(
            "cannot disable native updater: both {} and {} exist",
            source.display(),
            destination.display()
        );
    }
    match fs::rename(&source, &destination) {
        Ok(()) => Ok(()),
        // Another simultaneous launch may have completed the same rename.
        Err(e) if e.kind() == io::ErrorKind::NotFound && destination.is_file() => Ok(()),
        Err(e) => Err(e)
            .with_context(|| format!("renaming {} to {}", source.display(), destination.display())),
    }
}

/// MSIX ZIP part names are URI-escaped. Decode each component exactly once,
/// before safe_join validates the resulting filesystem path. Encoded separators
/// must not turn one package component into multiple filesystem components.
fn decode_package_path(name: &str) -> Result<String> {
    let mut components = Vec::new();
    for component in name.split('/') {
        let mut decoded = Vec::with_capacity(component.len());
        let mut bytes = component.bytes();
        while let Some(byte) = bytes.next() {
            if byte == b'%' {
                let high = bytes.next().and_then(|b| (b as char).to_digit(16));
                let low = bytes.next().and_then(|b| (b as char).to_digit(16));
                match (high, low) {
                    (Some(high), Some(low)) => decoded.push((high * 16 + low) as u8),
                    _ => bail!("invalid percent escape in package path: {}", name),
                }
            } else {
                decoded.push(byte);
            }
        }
        let decoded = String::from_utf8(decoded).context("package path is not valid UTF-8")?;
        if decoded.contains(['/', '\\', ':', '\0']) {
            bail!(
                "invalid separator or reserved character in package path: {}",
                name
            );
        }
        components.push(decoded);
    }
    Ok(components.join("/"))
}

/// Reject absolute paths, drive letters, and `..` traversal. ZIP entries
/// are untrusted input and the Store MSIX is signed but we still validate.
fn safe_join(base: &Path, rel: &str) -> Result<PathBuf> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        bail!("zip entry has absolute path: {}", rel);
    }
    let mut out = base.to_path_buf();
    for comp in rel_path.components() {
        use std::path::Component::*;
        match comp {
            Normal(c) => out.push(c),
            CurDir => {}
            ParentDir => bail!("zip entry escapes base via '..': {}", rel),
            Prefix(_) | RootDir => bail!("zip entry has root/prefix component: {}", rel),
        }
    }
    Ok(out)
}

/// Keep the `keep` newest versions (by semver-ish numeric sort of directory
/// names) under `<install_root>/versions/`, delete the rest. Also cleans up
/// stale `*.partial` directories. Returns the list of removed directory names.
pub fn prune_versions(install_root: &Path, keep: u32) -> Result<Vec<String>> {
    let versions_dir = install_root.join("versions");
    if !versions_dir.exists() {
        return Ok(Vec::new());
    }

    let mut versions: Vec<(Vec<u64>, String, PathBuf)> = Vec::new();
    let mut removed: Vec<String> = Vec::new();

    for entry in fs::read_dir(&versions_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = match entry.file_name().to_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        // Skip reparse points (e.g. the `versions/current` junction).
        // `remove_dir_all` on a junction would recurse into the target.
        if entry.file_type().map(|t| t.is_symlink()).unwrap_or(false) {
            continue;
        }
        if name.ends_with(".partial") {
            let _ = fs::remove_dir_all(&path);
            removed.push(name);
            continue;
        }
        let parts = parse_version(&name);
        versions.push((parts, name, path));
    }

    // Sort descending — newest first.
    versions.sort_by(|a, b| b.0.cmp(&a.0));

    let keep = keep.max(1) as usize;
    for (_, name, path) in versions.into_iter().skip(keep) {
        if let Err(e) = fs::remove_dir_all(&path) {
            eprintln!("warn: failed to remove {}: {}", path.display(), e);
            continue;
        }
        removed.push(name);
    }
    Ok(removed)
}

/// Windows AV (Defender) commonly holds transient handles on freshly-written
/// executables, causing `rename` of a directory tree to fail with ACCESS_DENIED.
/// Retry a few times with backoff before giving up.
fn rename_with_retry(from: &Path, to: &Path) -> io::Result<()> {
    let mut delay_ms = 100u64;
    for attempt in 0..6 {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if attempt < 5 => {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                delay_ms *= 2;
                let _ = e;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

fn parse_version(s: &str) -> Vec<u64> {
    s.split('.')
        .map(|p| p.parse::<u64>().unwrap_or(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_encoded_package_entries_to_decoded_paths() -> Result<()> {
        use std::io::Write;
        use std::time::{SystemTime, UNIX_EPOCH};
        use zip::write::SimpleFileOptions;

        let root = std::env::temp_dir().join(format!(
            "codex-extract-test-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        fs::create_dir(&root)?;
        let result = (|| -> Result<()> {
            let package = root.join("test.msix");
            let mut archive = zip::ZipWriter::new(fs::File::create(&package)?);
            for name in [
                "app/Codex.exe",
                "app/resources/node_modules/%40oai/cua/index.js",
                "app/resources/native/windows-updater.node",
            ] {
                archive.start_file(name, SimpleFileOptions::default())?;
                archive.write_all(b"fixture")?;
            }
            archive.finish()?;
            let installed = extract_app(&package, &root, "1.0", &mut |_, _| {})?;
            assert_eq!(
                fs::read(installed.join("resources/node_modules/@oai/cua/index.js"))?,
                b"fixture"
            );
            assert!(!installed.join("resources/node_modules/%40oai").exists());
            assert!(!installed
                .join("resources/native/windows-updater.node")
                .exists());
            assert_eq!(
                fs::read(installed.join("resources/native/windows-updater.broken"))?,
                b"fixture"
            );
            Ok(())
        })();
        fs::remove_dir_all(&root)?;
        result
    }
    #[test]
    fn decodes_scoped_packages_and_bundled_dependency_paths() {
        for (encoded, expected) in [
            ("resources/node_modules/%40oai/cua/index.js", "resources/node_modules/@oai/cua/index.js"),
            (".pnpm/%40rollup_plugin-typescript%4012.1.2_rollup%404.35.0_tslib%402.8.1_typescript%405.7.3/node_modules/tslib/tslib.es6.js",
             ".pnpm/@rollup_plugin-typescript@12.1.2_rollup@4.35.0_tslib@2.8.1_typescript@5.7.3/node_modules/tslib/tslib.es6.js"),
            ("assets/caf%C3%A9%20logo.png", "assets/caf\u{e9} logo.png"),
        ] {
            let decoded = decode_package_path(encoded).unwrap();
            assert_eq!(decoded, expected);
            assert_eq!(safe_join(Path::new("root"), &decoded).unwrap(), Path::new("root").join(expected));
        }
    }

    #[test]
    fn decodes_once_and_preserves_unescaped_names() {
        assert_eq!(
            decode_package_path("%2540oai/a+b.js").unwrap(),
            "%40oai/a+b.js"
        );
        assert_eq!(
            decode_package_path("plain/directory/").unwrap(),
            "plain/directory/"
        );
    }

    #[test]
    fn rejects_malformed_escapes_and_invalid_utf8() {
        for name in ["bad%", "bad%4", "bad%GG", "%FF"] {
            assert!(decode_package_path(name).is_err(), "{name}");
        }
    }

    #[test]
    fn rejects_encoded_separators_and_windows_reserved_characters() {
        for name in [
            "a%2fb",
            "a%5Cb",
            "C%3A/file",
            "file%00",
            "file:stream",
            "a\\b",
        ] {
            assert!(decode_package_path(name).is_err(), "{name}");
        }
    }

    #[test]
    fn validates_traversal_after_decoding() {
        for name in ["%2e%2e/escape", "folder/%2E%2E/escape", "/absolute"] {
            let decoded = decode_package_path(name).unwrap();
            assert!(safe_join(Path::new("root"), &decoded).is_err(), "{name}");
        }
    }
}
