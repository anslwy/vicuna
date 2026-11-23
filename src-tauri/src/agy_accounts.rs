// Extra Antigravity (`agy`) accounts. agy has no config-folder flag or env
// var: its login lives in `~/.gemini/antigravity-cli/antigravity-oauth-token`
// (and on macOS, macOS Keychain via `~/Library/Keychains/login.keychain-db`)
// plus `~/.config/antigravity/`. So every extra account gets its own home
// folder (`~/vicuna/accounts/agy/<id>/`) holding its own `.gemini/`,
// `.config/antigravity/`, and (on macOS) its own `Library/Keychains/` and
// `Library/Application Support/{antigravity,Antigravity}`, plus symlinks back
// to everything else in the real home (`.gitconfig`, `.ssh`, `.npmrc`, …) so
// the commands agy runs still see the user's normal setup. agy processes for
// that account run with `HOME=<account home>`. Vicuna never reads or copies
// credentials: sign-in is agy's own flow, run by the user as
// `HOME=<account home> agy`.
//
// The default account (the user's normal `~/.gemini` login) is plain
// `Engine::Agy`; extra accounts are `Engine::AgyAccount(<id>)` with the
// engine id `agy@<id>`, so sessions, model lists, saved models and usage are
// all keyed per account. Free and paid accounts list different models, which
// is why each account keeps its own model cache and saved pick.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::opencode::home_dir;

pub const ENGINE_PREFIX: &str = "agy@";
/// Under the account's `.gemini/` (agy 1.5.14).
const TOKEN_FILE: &str = "antigravity-cli/antigravity-oauth-token";
const MAX_ACCOUNTS: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Account {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountInfo {
    pub id: String,
    pub engine: String,
    pub label: String,
    pub home: String,
    pub signed_in: bool,
    pub sign_in_command: String,
}

// ---------- paths ----------

fn config_path_in(home: &Path) -> PathBuf {
    home.join("vicuna").join("config").join("agy-accounts.json")
}

pub fn account_home_in(home: &Path, id: &str) -> PathBuf {
    home.join("vicuna").join("accounts").join("agy").join(id)
}

pub fn account_home(id: &str) -> Option<PathBuf> {
    Some(account_home_in(&home_dir()?, id))
}

pub fn model_prefs_path(id: &str) -> Option<PathBuf> {
    Some(home_dir()?.join("vicuna").join("config").join(format!("agy-work@{id}.json")))
}

// ---------- registry ----------

pub fn read_in(home: &Path) -> Vec<Account> {
    std::fs::read_to_string(config_path_in(home))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("accounts").cloned())
        .and_then(|a| serde_json::from_value::<Vec<Account>>(a).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|a| valid_id(&a.id))
        .collect()
}

fn write_in(home: &Path, accounts: &[Account]) -> Result<(), String> {
    let path = config_path_in(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(&json!({ "accounts": accounts })).unwrap_or_default();
    std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

pub fn list() -> Vec<Account> {
    home_dir().map(|h| read_in(&h)).unwrap_or_default()
}

pub fn find(id: &str) -> Option<Account> {
    list().into_iter().find(|a| a.id == id)
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// "Work (paid)" → "work-paid"; never empty.
pub fn slug(label: &str) -> String {
    let mut s = String::new();
    for c in label.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c);
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
    }
    let s: String = s.trim_end_matches('-').chars().take(24).collect();
    let s = s.trim_end_matches('-').to_string();
    if s.is_empty() { "account".to_string() } else { s }
}

pub fn clean_label(label: &str) -> Result<String, String> {
    let l: String = label.trim().chars().filter(|c| !c.is_control()).collect();
    if l.is_empty() {
        return Err("Give the account a name, e.g. Work or Free.".into());
    }
    if l.chars().count() > 40 {
        return Err("Account names are at most 40 characters.".into());
    }
    Ok(l)
}

pub fn add_in(home: &Path, label: &str) -> Result<Account, String> {
    if cfg!(windows) {
        return Err("Extra Antigravity accounts are not supported on Windows yet.".into());
    }
    let label = clean_label(label)?;
    let mut accounts = read_in(home);
    if accounts.len() >= MAX_ACCOUNTS {
        return Err(format!("At most {MAX_ACCOUNTS} extra Antigravity accounts."));
    }
    if accounts.iter().any(|a| a.label.eq_ignore_ascii_case(&label)) || label.eq_ignore_ascii_case("default") {
        return Err(format!("An Antigravity account named \"{label}\" already exists."));
    }
    let base = slug(&label);
    let mut id = base.clone();
    let mut n = 2;
    // a removed account's folder is reused on purpose: same name, same login
    while accounts.iter().any(|a| a.id == id) {
        id = format!("{base}-{n}");
        n += 1;
    }
    let acct = Account { id, label };
    prepare_home(home, &account_home_in(home, &acct.id))?;
    accounts.push(acct.clone());
    write_in(home, &accounts)?;
    Ok(acct)
}

/// Changes only the display label. The id (folder, login, saved model and
/// engine sessions) stays, so a renamed account needs no new sign-in.
pub fn rename_in(home: &Path, id: &str, label: &str) -> Result<Account, String> {
    let label = clean_label(label)?;
    let mut accounts = read_in(home);
    if label.eq_ignore_ascii_case("default") || accounts.iter().any(|a| a.id != id && a.label.eq_ignore_ascii_case(&label)) {
        return Err(format!("An Antigravity account named \"{label}\" already exists."));
    }
    let acct = accounts.iter_mut().find(|a| a.id == id).ok_or_else(|| format!("No Antigravity account \"{id}\"."))?;
    acct.label = label;
    let out = acct.clone();
    write_in(home, &accounts)?;
    Ok(out)
}

/// Removes the account from Vicuna only. Its folder (and agy's login in it)
/// stays on disk, so re-adding the same name signs straight back in.
pub fn remove_in(home: &Path, id: &str) -> Result<(), String> {
    let mut accounts = read_in(home);
    let before = accounts.len();
    accounts.retain(|a| a.id != id);
    if accounts.len() == before {
        return Err(format!("No Antigravity account \"{id}\"."));
    }
    write_in(home, &accounts)
}

// ---------- account home ----------

/// Build (or top up) an account home: own `.gemini/` and
/// `.config/antigravity/` (plus on macOS own `Library/Keychains/` and
/// `Library/Application Support/{antigravity,Antigravity}` so macOS Keychain
/// `keyringAuth` does not read the user's default login), and a symlink for
/// every other entry of the real home (and of the real `~/.config` /
/// `~/Library`). Existing entries are left alone, so this is cheap to run
/// before every spawn and picks up new dotfiles.
pub fn prepare_home(real_home: &Path, acct_home: &Path) -> Result<(), String> {
    let mk = |p: &Path| std::fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {e}", p.display()));
    mk(&acct_home.join(".gemini"))?;
    mk(&acct_home.join(".config").join("antigravity"))?;
    // `vicuna` holds the account homes themselves: linking it would loop.
    // On macOS, `Library` holds `Keychains/login.keychain-db` (read by agy's
    // `keyringAuth` via `$HOME/Library/Keychains`) and `Application Support`.
    let skip_root: &[&str] = if cfg!(target_os = "macos") {
        &[".gemini", ".config", "vicuna", "Library"]
    } else {
        &[".gemini", ".config", "vicuna"]
    };
    link_children(real_home, acct_home, skip_root);
    link_children(&real_home.join(".config"), &acct_home.join(".config"), &["antigravity"]);
    #[cfg(target_os = "macos")]
    prepare_macos_library(real_home, acct_home)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn prepare_macos_library(real_home: &Path, acct_home: &Path) -> Result<(), String> {
    let mk = |p: &Path| std::fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {e}", p.display()));
    let lib = acct_home.join("Library");
    // Migrate account homes created before Library isolation (where `Library`
    // was a symlink straight to the user's real `~/Library`).
    if lib.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
        std::fs::remove_file(&lib).map_err(|e| format!("cannot unlink {}: {e}", lib.display()))?;
    }
    mk(&lib.join("Keychains"))?;
    let app_sup = lib.join("Application Support");
    mk(&app_sup.join("antigravity"))?;
    mk(&app_sup.join("Antigravity"))?;
    let real_lib = real_home.join("Library");
    link_children(&real_lib, &lib, &["Keychains", "Application Support"]);
    link_children(&real_lib.join("Application Support"), &app_sup, &["antigravity", "Antigravity"]);
    ensure_keychain(&account_keychain(acct_home));
    Ok(())
}

/// The account's own keychain. macOS makes `$HOME/Library/Keychains/login.keychain-db`
/// the default keychain for that HOME, so agy's `keyringAuth` stores the login here.
#[cfg(target_os = "macos")]
fn account_keychain(acct_home: &Path) -> PathBuf {
    acct_home.join("Library").join("Keychains").join("login.keychain-db")
}

/// Create the account keychain (empty password, no auto-lock) if missing, and
/// unlock it. Without one, agy's first save pops macOS's "Keychain Not Found /
/// Reset To Defaults" dialog (Cancel falls back to the token file).
/// Best-effort: failures only bring that dialog back.
#[cfg(target_os = "macos")]
fn ensure_keychain(kc: &Path) {
    let security = |args: &[&std::ffi::OsStr], home: Option<&Path>| {
        let mut c = std::process::Command::new("/usr/bin/security");
        c.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        // A scratch HOME keeps `security` from touching the user's real
        // preferences (the account home's `Library/Preferences` links to them).
        if let Some(h) = home {
            c.env("HOME", h);
        }
        c.status().is_ok_and(|s| s.success())
    };
    let kc_os = kc.as_os_str();
    let empty = std::ffi::OsStr::new("");
    if !kc.exists() {
        let scratch = std::env::temp_dir().join(format!("vicuna-agy-keychain-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&scratch);
        let made = security(&["create-keychain".as_ref(), "-p".as_ref(), empty, kc_os], Some(&scratch));
        let _ = std::fs::remove_dir_all(&scratch);
        if !made {
            return;
        }
        // No arguments = never auto-lock (no timeout, no lock on sleep).
        security(&["set-keychain-settings".as_ref(), kc_os], None);
    }
    security(&["unlock-keychain".as_ref(), "-p".as_ref(), empty, kc_os], None);
}

/// agy keeps its macOS keychain login as generic password service `gemini`,
/// account `antigravity`. Checks attributes only; the secret is never read.
#[cfg(target_os = "macos")]
fn keychain_has_login(acct_home: &Path) -> bool {
    let kc = account_keychain(acct_home);
    if !kc.is_file() {
        return false;
    }
    std::process::Command::new("/usr/bin/security")
        .args(["unlock-keychain", "-p", ""])
        .arg(&kc)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok();
    std::process::Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", "gemini", "-a", "antigravity"])
        .arg(&kc)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn link_children(from: &Path, to: &Path, skip: &[&str]) {
    let Ok(rd) = std::fs::read_dir(from) else { return };
    for e in rd.flatten() {
        let name = e.file_name();
        if skip.iter().any(|s| name == *s) {
            continue;
        }
        let dest = to.join(&name);
        if dest.symlink_metadata().is_ok() {
            continue;
        }
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink(e.path(), &dest);
    }
}

pub fn signed_in_at(acct_home: &Path) -> bool {
    if acct_home.join(".gemini").join(TOKEN_FILE).is_file() {
        return true;
    }
    #[cfg(target_os = "macos")]
    if keychain_has_login(acct_home) {
        return true;
    }
    false
}

pub fn signed_in(id: &str) -> bool {
    account_home(id).is_some_and(|h| signed_in_at(&h))
}

/// Environment for an agy process on this account.
pub fn env_for(id: &str) -> Result<Vec<(String, String)>, String> {
    let real = home_dir().ok_or("cannot locate home folder")?;
    let home = account_home_in(&real, id);
    prepare_home(&real, &home)?;
    let h = home.display().to_string();
    Ok(vec![
        ("HOME".into(), h.clone()),
        ("XDG_CONFIG_HOME".into(), format!("{h}/.config")),
    ])
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn sign_in_command_for(acct_home: &Path) -> String {
    format!("HOME={} agy", shell_quote(&acct_home.display().to_string()))
}

pub fn info_in(home: &Path, a: &Account) -> AccountInfo {
    let h = account_home_in(home, &a.id);
    AccountInfo {
        id: a.id.clone(),
        engine: format!("{ENGINE_PREFIX}{}", a.id),
        label: a.label.clone(),
        home: h.display().to_string(),
        signed_in: signed_in_at(&h),
        sign_in_command: sign_in_command_for(&h),
    }
}

pub fn not_signed_in_message(id: &str) -> String {
    let label = find(id).map(|a| a.label).unwrap_or_else(|| id.to_string());
    let cmd = account_home(id).map(|h| sign_in_command_for(&h)).unwrap_or_else(|| "agy".into());
    format!("Antigravity account \"{label}\" is not signed in: run `{cmd}` in a terminal on this machine and sign in with that Google account.")
}

// ---------- API actions ----------

pub fn list_logic() -> Result<Value, String> {
    let home = home_dir().ok_or("cannot locate home folder")?;
    let accounts: Vec<AccountInfo> = read_in(&home)
        .iter()
        .map(|a| {
            let _ = prepare_home(&home, &account_home_in(&home, &a.id));
            info_in(&home, a)
        })
        .collect();
    Ok(json!({ "accounts": accounts, "supported": !cfg!(windows) }))
}

pub fn add_logic(label: &str) -> Result<AccountInfo, String> {
    let home = home_dir().ok_or("cannot locate home folder")?;
    let a = add_in(&home, label)?;
    Ok(info_in(&home, &a))
}

pub fn rename_logic(id: &str, label: &str) -> Result<AccountInfo, String> {
    let home = home_dir().ok_or("cannot locate home folder")?;
    let a = rename_in(&home, id, label)?;
    Ok(info_in(&home, &a))
}

pub fn remove_logic(id: &str) -> Result<Value, String> {
    let home = home_dir().ok_or("cannot locate home folder")?;
    remove_in(&home, id)?;
    Ok(json!({ "removed": id, "keptFolder": account_home_in(&home, id).display().to_string() }))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let d = std::env::temp_dir().join(format!("vicuna-agyacct-{tag}-{n}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn slugs_and_labels() {
        assert_eq!(slug("Work (paid)"), "work-paid");
        assert_eq!(slug("  ✨ "), "account");
        assert!(valid_id("work-paid") && !valid_id("../x") && !valid_id(""));
        assert!(clean_label("  ").is_err());
        assert_eq!(clean_label(" Free ").unwrap(), "Free");
    }

    #[test]
    fn add_list_remove_roundtrip() {
        let home = tmp("reg");
        let a = add_in(&home, "Free").unwrap();
        assert_eq!(a.id, "free");
        assert!(add_in(&home, "free").is_err(), "duplicate label");
        assert!(add_in(&home, "Default").is_err(), "reserved");
        let b = add_in(&home, "Work paid").unwrap();
        assert_eq!(read_in(&home).iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["free", "work-paid"]);
        assert!(!info_in(&home, &b).signed_in);
        // rename keeps the id (folder + login); labels stay unique
        assert!(rename_in(&home, "work-paid", "free").is_err(), "clashes with another label");
        assert!(rename_in(&home, "work-paid", "default").is_err(), "reserved");
        assert!(rename_in(&home, "nope", "X").is_err());
        let b = rename_in(&home, "work-paid", "Pro").unwrap();
        assert_eq!((b.id.as_str(), b.label.as_str()), ("work-paid", "Pro"));
        assert_eq!(rename_in(&home, "work-paid", "pro").unwrap().label, "pro", "case-only change of its own name");
        let b = rename_in(&home, "work-paid", "Pro").unwrap();
        remove_in(&home, "free").unwrap();
        assert!(remove_in(&home, "free").is_err());
        assert_eq!(read_in(&home), vec![b]);
        // the folder stays (login survives a remove)
        assert!(account_home_in(&home, "free").join(".gemini").is_dir());
    }

    #[test]
    fn account_home_isolates_login_and_links_the_rest() {
        let real = tmp("real");
        std::fs::create_dir_all(real.join(".gemini").join("antigravity-cli")).unwrap();
        std::fs::write(real.join(".gemini").join(TOKEN_FILE), "secret").unwrap();
        std::fs::create_dir_all(real.join(".config").join("antigravity")).unwrap();
        std::fs::create_dir_all(real.join(".config").join("git")).unwrap();
        std::fs::write(real.join(".gitconfig"), "[user]").unwrap();
        std::fs::create_dir_all(real.join(".ssh")).unwrap();
        let acct = account_home_in(&real, "work");
        prepare_home(&real, &acct).unwrap();
        // own login folders, not links to the real ones
        assert!(!acct.join(".gemini").symlink_metadata().unwrap().file_type().is_symlink());
        assert!(!signed_in_at(&acct), "the real login must not leak into the account");
        assert!(!acct.join(".config").join("antigravity").symlink_metadata().unwrap().file_type().is_symlink());
        // everything else points back at the real home
        assert_eq!(std::fs::read_to_string(acct.join(".gitconfig")).unwrap(), "[user]");
        assert!(acct.join(".ssh").symlink_metadata().unwrap().file_type().is_symlink());
        assert!(acct.join(".config").join("git").symlink_metadata().unwrap().file_type().is_symlink());
        // idempotent, and a signed-in account is detected
        prepare_home(&real, &acct).unwrap();
        std::fs::create_dir_all(acct.join(".gemini").join("antigravity-cli")).unwrap();
        std::fs::write(acct.join(".gemini").join(TOKEN_FILE), "t").unwrap();
        assert!(signed_in_at(&acct));
        assert!(sign_in_command_for(&acct).starts_with("HOME='") && sign_in_command_for(&acct).ends_with("' agy"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_isolates_keychains_and_migrates_library_symlink() {
        let real = tmp("mac-real");
        std::fs::create_dir_all(real.join("Library").join("Keychains")).unwrap();
        std::fs::write(real.join("Library").join("Keychains").join("login.keychain-db"), "secret").unwrap();
        std::fs::create_dir_all(real.join("Library").join("Application Support").join("antigravity")).unwrap();
        std::fs::write(real.join("Library").join("Application Support").join("antigravity").join("state"), "s").unwrap();
        std::fs::create_dir_all(real.join("Library").join("Preferences")).unwrap();
        std::fs::write(real.join("Library").join("Preferences").join("p.plist"), "ok").unwrap();

        let acct = account_home_in(&real, "free");
        std::fs::create_dir_all(&acct).unwrap();
        // simulate pre-fix account home with a symlinked Library
        std::os::unix::fs::symlink(real.join("Library"), acct.join("Library")).unwrap();

        prepare_home(&real, &acct).unwrap();
        assert!(!acct.join("Library").symlink_metadata().unwrap().file_type().is_symlink());
        assert!(!acct.join("Library").join("Keychains").symlink_metadata().unwrap().file_type().is_symlink());
        // The account gets its own (fresh) keychain, never the real one.
        let kc = acct.join("Library").join("Keychains").join("login.keychain-db");
        assert!(!kc.symlink_metadata().unwrap().file_type().is_symlink());
        assert_ne!(std::fs::read(&kc).unwrap(), b"secret");
        assert!(!signed_in_at(&acct), "an empty account keychain is not a login");
        assert!(!acct.join("Library").join("Application Support").join("antigravity").join("state").exists());
        assert!(acct.join("Library").join("Preferences").symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(acct.join("Library").join("Preferences").join("p.plist")).unwrap(), "ok");
    }
}
