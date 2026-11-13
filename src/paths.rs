use std::path::PathBuf;

pub const APP_NAME: &str = "BearDL";

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

pub fn home() -> PathBuf {
    env_path("HOME")
        .or_else(|| env_path("USERPROFILE"))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `BEARDL_HOME` overrides every location, which keeps tests and portable installs self-contained.
fn portable(sub: &str) -> Option<PathBuf> {
    env_path("BEARDL_HOME").map(|p| p.join(sub))
}

/// Settings and themes.
pub fn config_dir() -> PathBuf {
    if let Some(p) = portable("config") {
        return p;
    }
    let base = if cfg!(windows) {
        env_path("APPDATA").unwrap_or_else(home)
    } else if cfg!(target_os = "macos") {
        home().join("Library/Application Support")
    } else {
        env_path("XDG_CONFIG_HOME").unwrap_or_else(|| home().join(".config"))
    };
    base.join(APP_NAME)
}

/// Database and downloaded tools.
pub fn data_dir() -> PathBuf {
    if let Some(p) = portable("data") {
        return p;
    }
    let base = if cfg!(windows) {
        env_path("LOCALAPPDATA").unwrap_or_else(home)
    } else if cfg!(target_os = "macos") {
        home().join("Library/Application Support")
    } else {
        env_path("XDG_DATA_HOME").unwrap_or_else(|| home().join(".local/share"))
    };
    base.join(APP_NAME)
}

pub fn cache_dir() -> PathBuf {
    if let Some(p) = portable("cache") {
        return p;
    }
    if cfg!(windows) {
        data_dir().join("cache")
    } else if cfg!(target_os = "macos") {
        home().join("Library/Caches").join(APP_NAME)
    } else {
        env_path("XDG_CACHE_HOME")
            .unwrap_or_else(|| home().join(".cache"))
            .join(APP_NAME)
    }
}

pub fn tools_dir() -> PathBuf {
    data_dir().join("tools")
}

pub fn thumbs_dir() -> PathBuf {
    cache_dir().join("thumbs")
}

pub fn themes_dir() -> PathBuf {
    config_dir().join("themes")
}

pub fn db_path() -> PathBuf {
    data_dir().join("library.sqlite")
}

pub fn settings_path() -> PathBuf {
    config_dir().join("settings.toml")
}

pub fn default_download_dir() -> PathBuf {
    let h = home();
    let d = h.join("Downloads");
    if d.is_dir() {
        d.join(APP_NAME)
    } else {
        h.join(APP_NAME)
    }
}

pub fn ensure_dirs() -> std::io::Result<()> {
    for d in [
        config_dir(),
        data_dir(),
        tools_dir(),
        thumbs_dir(),
        themes_dir(),
    ] {
        std::fs::create_dir_all(d)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn tools_live_under_data() {
        assert!(super::tools_dir().starts_with(super::data_dir()));
        assert!(super::db_path().starts_with(super::data_dir()));
    }
}
