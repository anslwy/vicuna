// Extra Claude Code and Codex accounts use the CLIs' own config directories.
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use tokio::process::Command;

use crate::{
    agy_accounts::{Account, AccountInfo, clean_label, shell_quote, slug, valid_id},
    opencode::{WorkEngineInfo, home_dir},
    work_engine::Engine,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Harness {
    Claude,
    Codex,
}

impl Harness {
    pub fn parse(id: &str) -> Option<Self> {
        match id {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }

    pub fn base(self) -> Engine {
        match self {
            Self::Claude => Engine::Claude,
            Self::Codex => Engine::Codex,
        }
    }

    fn config_env(self) -> &'static str {
        match self {
            Self::Claude => "CLAUDE_CONFIG_DIR",
            Self::Codex => "CODEX_HOME",
        }
    }

    fn credential_overrides(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &[
                "CLAUDE_SECURESTORAGE_CONFIG_DIR",
                "CLAUDE_CODE_OAUTH_TOKEN",
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
                "ANTHROPIC_BASE_URL",
                "CLAUDE_CODE_USE_BEDROCK",
                "CLAUDE_CODE_USE_VERTEX",
                "CLAUDE_CODE_USE_FOUNDRY",
                "CLAUDE_CODE_USE_GATEWAY",
            ],
            Self::Codex => &[
                "CODEX_API_KEY",
                "CODEX_ACCESS_TOKEN",
                "OPENAI_API_KEY",
                "OPENAI_BASE_URL",
            ],
        }
    }

    fn login_args(self) -> &'static str {
        match self {
            Self::Claude => "auth login",
            Self::Codex => "login",
        }
    }

    fn registry(self, home: &Path) -> PathBuf {
        home.join("vicuna")
            .join("config")
            .join(format!("{}-accounts.json", self.base().id()))
    }

    fn profile(self, home: &Path, id: &str) -> PathBuf {
        home.join("vicuna")
            .join("accounts")
            .join(self.base().id())
            .join(id)
    }

    pub fn model_prefs(self, id: &str) -> Option<PathBuf> {
        Some(
            home_dir()?
                .join("vicuna")
                .join("config")
                .join(format!("{}-work@{id}.json", self.base().id())),
        )
    }

    fn read(self, home: &Path) -> Result<Vec<Account>, String> {
        match fs::read_to_string(self.registry(home)) {
            Ok(text) => {
                serde_json::from_str(&text).map_err(|e| format!("Invalid account registry: {e}"))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(format!("Cannot read account registry: {e}")),
        }
    }

    fn write(self, home: &Path, accounts: &[Account]) -> Result<(), String> {
        let path = self.registry(home);
        fs::create_dir_all(path.parent().expect("registry has a parent"))
            .map_err(|e| e.to_string())?;
        let temp = path.with_extension("tmp");
        fs::write(
            &temp,
            serde_json::to_vec_pretty(accounts).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        fs::rename(temp, path).map_err(|e| e.to_string())
    }

    pub fn list(self) -> Vec<Account> {
        home_dir()
            .and_then(|home| self.read(&home).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|a| valid_id(&a.id))
            .collect()
    }

    pub fn find(self, id: &str) -> Option<Account> {
        self.list().into_iter().find(|a| a.id == id)
    }

    fn checked_profile(self, id: &str) -> Result<PathBuf, String> {
        if self.find(id).is_none() {
            return Err(format!(
                "The {} account \"{id}\" was removed. Pick another account for this session.",
                self.base().label()
            ));
        }
        let dir = self.profile(&home_dir().ok_or("Cannot locate home folder.")?, id);
        if !dir.is_dir() {
            return Err(format!("Account directory is missing: {}", dir.display()));
        }
        Ok(dir)
    }

    pub fn environment(
        self,
        id: &str,
    ) -> Result<(&'static [&'static str], (&'static str, PathBuf)), String> {
        Ok((
            self.credential_overrides(),
            (self.config_env(), self.checked_profile(id)?),
        ))
    }

    pub fn configure(self, id: &str, cmd: &mut Command) -> Result<(), String> {
        let (clear, (key, dir)) = self.environment(id)?;
        for var in clear {
            cmd.env_remove(var);
        }
        cmd.env(key, dir);
        Ok(())
    }

    fn sign_in_command(self, dir: &Path) -> String {
        let bin = self.base().id();
        let var = self.config_env();
        let path = dir.to_string_lossy();
        if cfg!(windows) {
            let clear = self
                .credential_overrides()
                .iter()
                .map(|k| format!("$env:{k} = $null; "))
                .collect::<String>();
            format!(
                "{clear}$env:{var} = '{}'; {bin} {}",
                path.replace('\'', "''"),
                self.login_args()
            )
        } else {
            let clear = self.credential_overrides().join(" -u ");
            format!(
                "env -u {clear} {var}={} {bin} {}",
                shell_quote(&path),
                self.login_args()
            )
        }
    }

    fn info(self, home: &Path, account: &Account, signed_in: bool) -> AccountInfo {
        let dir = self.profile(home, &account.id);
        AccountInfo {
            id: account.id.clone(),
            engine: format!("{}@{}", self.base().id(), account.id),
            label: account.label.clone(),
            home: dir.display().to_string(),
            signed_in,
            sign_in_command: self.sign_in_command(&dir),
        }
    }

    pub fn sign_in_hint(self, id: &str) -> String {
        let Some(home) = home_dir() else {
            return "Cannot locate home folder.".into();
        };
        format!(
            "To sign in to this {} account, run `{}` in a terminal on this machine.",
            self.base().label(),
            self.sign_in_command(&self.profile(&home, id))
        )
    }

    pub async fn status(self, id: &str, bin: &Path) -> Option<serde_json::Value> {
        let mut cmd = Command::new(bin);
        let (clear, (key, dir)) = self.environment(id).ok()?;
        for var in clear {
            cmd.env_remove(var);
        }
        cmd.env(key, &dir)
            .current_dir(dir)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        match self {
            Self::Claude => cmd.args(["auth", "status"]),
            Self::Codex => cmd.args(["login", "status"]),
        };
        #[cfg(windows)]
        cmd.creation_flags(0x08000000);
        let out = tokio::time::timeout(Duration::from_secs(3), cmd.output())
            .await
            .ok()?
            .ok()?;
        if !out.status.success() {
            return None;
        }
        match self {
            Self::Claude => {
                let value: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
                (value.get("loggedIn").and_then(|v| v.as_bool()) == Some(true)).then_some(value)
            }
            Self::Codex => Some(serde_json::Value::Null),
        }
    }

    pub async fn engine_info(
        self,
        id: &str,
        base: &WorkEngineInfo,
    ) -> (WorkEngineInfo, Option<AccountInfo>) {
        let mut info = base.clone();
        let engine = Engine::CliAccount(self, crate::plugin_harness::intern(id));
        info.id = engine.id().to_owned();
        info.label = engine.label().to_owned();
        info.model = engine.saved_model();
        let account = if let (Some(home), Some(account)) = (home_dir(), self.find(id)) {
            let signed_in = match &info.path {
                Some(bin) => self.status(id, Path::new(bin)).await.is_some(),
                None => false,
            };
            let account = self.info(&home, &account, signed_in);
            info.config_path = Some(account.home.clone());
            if info.installed && !signed_in {
                info.warning = Some(self.sign_in_hint(id));
            }
            Some(account)
        } else {
            info.installed = false;
            info.missing = Some(engine.missing_message());
            None
        };
        (info, account)
    }
}

static REGISTRY: Mutex<()> = Mutex::new(());

pub fn mutate(
    harness: Harness,
    action: &str,
    id: &str,
    label: &str,
) -> Result<Option<AccountInfo>, String> {
    let home = home_dir().ok_or("Cannot locate home folder.")?;
    let _guard = REGISTRY.lock().map_err(|e| e.to_string())?;
    let mut accounts = harness.read(&home)?;
    if action == "work_account_remove" {
        let index = accounts
            .iter()
            .position(|a| a.id == id)
            .ok_or("Account was removed.")?;
        accounts.remove(index);
        harness.write(&home, &accounts)?;
        return Ok(None);
    }

    let label = clean_label(label)?;
    let renaming = action == "work_account_rename";
    if label.eq_ignore_ascii_case("default")
        || accounts
            .iter()
            .any(|a| (!renaming || a.id != id) && a.label.eq_ignore_ascii_case(&label))
    {
        return Err(format!("An account named \"{label}\" already exists."));
    }
    let account = if renaming {
        let account = accounts
            .iter_mut()
            .find(|a| a.id == id && valid_id(&a.id))
            .ok_or("Account was removed.")?;
        account.label = label;
        account.clone()
    } else {
        if accounts.len() >= 8 {
            return Err("At most eight extra accounts per harness.".into());
        }
        let base = slug(&label);
        let mut id = base.clone();
        let mut suffix = 2;
        let mut dir = harness.profile(&home, &id);
        // Retain removed profile directories and reserve their IDs so a new
        // login can never inherit the old account's pinned chats or sessions.
        while dir.exists() || accounts.iter().any(|a| a.id == id) {
            id = format!("{base}-{suffix}");
            dir = harness.profile(&home, &id);
            suffix += 1;
        }
        fs::create_dir_all(dir.parent().expect("profile has a parent"))
            .map_err(|e| e.to_string())?;
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&dir).map_err(|e| e.to_string())?;
        let account = Account { id, label };
        accounts.push(account.clone());
        account
    };
    harness.write(&home, &accounts)?;
    Ok(Some(harness.info(&home, &account, false)))
}
