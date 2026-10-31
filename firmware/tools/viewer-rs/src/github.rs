//! Updates from the project's GitHub releases: the newest release's tag, its firmware and its viewer.
//! The "latest" URLs redirect to the newest release, so no API (or JSON) is needed.

use std::io::Read;

const LATEST: &str = "https://github.com/StaRadian/sutterspeed/releases/latest";

/// "v0.3.8" / "0.3.8" -> (0, 3, 8)
pub fn version(s: &str) -> Option<(u32, u32, u32)> {
    let mut it = s.trim().trim_start_matches('v').split('.').map(|p| p.trim().parse::<u32>().ok());
    Some((it.next()??, it.next()??, it.next().flatten().unwrap_or(0)))
}

/// tag of the newest release, e.g. "v0.3.8"
pub fn latest_tag() -> Result<String, String> {
    let r = ureq::get(LATEST).call().map_err(|e| format!("GitHub: {e}"))?;
    let url = r.get_url().to_string(); // .../releases/tag/v0.3.8 after the redirect
    url.rsplit('/').next().filter(|t| version(t).is_some()).map(str::to_string).ok_or_else(|| format!("no release found ({url})"))
}

/// a file of the newest release
pub fn download(name: &str) -> Result<Vec<u8>, String> {
    let r = ureq::get(&format!("{LATEST}/download/{name}")).call().map_err(|e| format!("download {name}: {e}"))?;
    let mut v = Vec::new();
    r.into_reader().take(64 << 20).read_to_end(&mut v).map_err(|e| format!("download {name}: {e}"))?;
    Ok(v)
}

/// Put a new viewer in place of the running one. Windows lets a running program be renamed, not overwritten: the
/// old one moves aside (removed at the next start) and the new one takes its name.
pub fn replace_viewer(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < 1 << 20 || !bytes.starts_with(b"MZ") {
        return Err("the download is not a Windows program".into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let (new, old) = (exe.with_extension("new"), exe.with_extension("old"));
    std::fs::write(&new, bytes).map_err(|e| format!("cannot write {}: {e}", new.display()))?;
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| format!("cannot move the running viewer aside: {e}"))?;
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(format!("cannot put the new viewer in place: {e}"));
    }
    Ok(())
}

/// the viewer replaced by the last update
pub fn remove_old() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(exe.with_extension("old"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(version("v0.3.8"), Some((0, 3, 8)));
        assert!(version("v0.3.10") > version("0.3.9"));
        assert_eq!(version("main"), None);
    }

    /// needs the network: cargo test -- --ignored
    #[test]
    #[ignore]
    fn newest_release() {
        let tag = latest_tag().unwrap();
        assert!(version(&tag).is_some(), "{tag}");
        let hex = download("sutterspeed.hex").unwrap();
        assert!(crate::dfu::load_bytes(hex, true).is_ok());
    }
}
