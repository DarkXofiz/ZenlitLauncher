//! Fabric installer: Minecraft/Loader version lists, profile and library download,
//! and the matching Fabric API jar. All network work runs on worker threads and is
//! reported through `Shared`; the launcher page only reads that state each frame.
use super::pages::{Action, button};
use horizon_client::ui_theme::{Face, Ui};
use serde_json::Value;
use sha1::{Digest, Sha1};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

const META: &str = "https://meta.fabricmc.net/v2/versions";
const MAVEN: &str = "https://maven.fabricmc.net/";
const MODRINTH: &str = "https://api.modrinth.com/v2/project/fabric-api/version";
const USER_AGENT: &str = concat!("Horizon-Android/", env!("CARGO_PKG_VERSION"), " (fabric-installer)");
const MAX_JSON: u64 = 16 * 1024 * 1024;
const ROW_H: f32 = 40.0;

#[derive(Clone, Copy, PartialEq)]
enum Step {
    Game,
    Loader,
}

#[derive(Clone)]
struct Game {
    version: String,
    stable: bool,
}

#[derive(Clone)]
struct Loader {
    version: String,
    stable: bool,
}

#[derive(Default)]
struct Shared {
    games: Option<Vec<Game>>,
    loaders: Option<(String, Vec<Loader>)>,
    loading: bool,
    installing: bool,
    stage: String,
    fraction: f32,
    /// Last finished action: Ok(message) or Err(message).
    outcome: Option<Result<String, String>>,
    list_error: Option<String>,
}

pub(super) struct FabricPage {
    shared: Arc<Mutex<Shared>>,
    step: Step,
    snapshots: bool,
    game: Option<usize>,
    loader: Option<usize>,
    pub view: [f32; 3],
}

fn lock(shared: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

impl FabricPage {
    pub fn new() -> Self {
        Self {
            shared: Arc::default(),
            step: Step::Game,
            snapshots: false,
            game: None,
            loader: None,
            view: [0.0; 3],
        }
    }

    /// Called when the Fabric page is opened: loads the Minecraft list once.
    pub fn open(&mut self) {
        let need = {
            let s = lock(&self.shared);
            s.games.is_none() && !s.loading && !s.installing
        };
        if need {
            self.reload();
        }
    }

    pub fn reload(&mut self) {
        {
            let mut s = lock(&self.shared);
            if s.loading || s.installing {
                return;
            }
            s.loading = true;
            s.list_error = None;
            s.outcome = None;
        }
        let shared = self.shared.clone();
        let wanted = self.selected_game().map(|g| g.version.clone());
        spawn("FabricVersions", move || {
            let result = fetch_games();
            let mut s = lock(&shared);
            s.loading = false;
            match result {
                Ok(games) => s.games = Some(games),
                Err(e) => s.list_error = Some(e),
            }
            drop(s);
            if let Some(version) = wanted {
                load_loaders(&shared, version);
            }
        });
    }

    fn visible_games(&self) -> Vec<Game> {
        let s = lock(&self.shared);
        s.games
            .as_ref()
            .map(|g| {
                g.iter()
                    .filter(|g| self.snapshots || g.stable)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn selected_game(&self) -> Option<Game> {
        self.game.and_then(|i| self.visible_games().get(i).cloned())
    }

    pub fn toggle_snapshots(&mut self) {
        self.snapshots = !self.snapshots;
        self.game = None;
        self.loader = None;
        self.step = Step::Game;
    }

    pub fn select_game(&mut self, index: usize) {
        let Some(game) = self.visible_games().get(index).cloned() else {
            return;
        };
        self.game = Some(index);
        self.loader = None;
        self.step = Step::Loader;
        {
            let mut s = lock(&self.shared);
            if s.installing {
                return;
            }
            s.outcome = None;
            s.list_error = None;
            s.loading = true;
            s.loaders = None;
        }
        let shared = self.shared.clone();
        spawn("FabricLoaders", move || load_loaders(&shared, game.version));
    }

    pub fn select_loader(&mut self, index: usize) {
        self.loader = Some(index);
    }

    pub fn back(&mut self) {
        self.step = Step::Game;
        self.loader = None;
        lock(&self.shared).outcome = None;
    }

    pub fn dismiss(&mut self) {
        lock(&self.shared).outcome = None;
    }

    pub fn busy(&self) -> bool {
        let s = lock(&self.shared);
        s.loading || s.installing
    }

    /// Starts installing the selected pair into `game_home`.
    pub fn install(&mut self, game_home: PathBuf) {
        let Some(game) = self.selected_game() else {
            return;
        };
        let loader = {
            let s = lock(&self.shared);
            match (&s.loaders, self.loader) {
                (Some((v, list)), Some(i)) if *v == game.version => list.get(i).cloned(),
                _ => None,
            }
        };
        let Some(loader) = loader else {
            return;
        };
        {
            let mut s = lock(&self.shared);
            if s.installing || s.loading {
                return;
            }
            s.installing = true;
            s.fraction = 0.0;
            s.stage = "Starting...".into();
            s.outcome = None;
        }
        let shared = self.shared.clone();
        spawn("FabricInstall", move || {
            let result = run_install(&shared, &game_home, &game.version, &loader.version);
            let mut s = lock(&shared);
            s.installing = false;
            s.fraction = 1.0;
            s.outcome = Some(result);
        });
    }

    /// Draws the page below `top`; `area` is `[x, width, top]` and `self.view` carries the scroll area.
    pub fn draw(
        &mut self,
        ui: &mut Ui,
        targets: &mut Vec<([f32; 4], Action)>,
        scroll: f32,
        [x, w, top]: [f32; 3],
        game_home: &Path,
    ) {
        use horizon_client::ui_theme::{Icon, active_theme};
        let theme = active_theme();
        self.view = [top, top, 0.0];
        let games = self.visible_games();
        let selected_game = self.game.and_then(|i| games.get(i).cloned());
        let (installing, loading, stage, fraction, outcome, list_error, loaders) = {
            let s = lock(&self.shared);
            (
                s.installing,
                s.loading,
                s.stage.clone(),
                s.fraction,
                s.outcome.clone(),
                s.list_error.clone(),
                s.loaders.clone(),
            )
        };

        ui.text(Face::SemiBold, 19.0, x, top, theme.text_primary, "Fabric");
        let sub = ui.fit_text(
            Face::Regular,
            11.5,
            "Installs the Fabric Loader profile and a matching Fabric API into the active profile.",
            w,
        );
        ui.text(Face::Regular, 11.5, x, top + 24.0, theme.text_muted, &sub);

        // Installing: progress card replaces the lists.
        if installing {
            let y = top + 60.0;
            ui.rounded(x, y, w, 96.0, 12.0, theme.surface_dim);
            ui.border(x, y, w, 96.0, 12.0, theme.border);
            ui.text(Face::SemiBold, 14.0, x + 16.0, y + 14.0, theme.text_primary, "Installing Fabric");
            let line = ui.fit_text(Face::Regular, 12.0, &stage, w - 32.0);
            ui.text(Face::Regular, 12.0, x + 16.0, y + 38.0, theme.text_muted, &line);
            let bar_w = w - 32.0;
            ui.rounded(x + 16.0, y + 66.0, bar_w, 8.0, 4.0, theme.border);
            ui.rounded(
                x + 16.0,
                y + 66.0,
                (bar_w * fraction.clamp(0.02, 1.0)).max(8.0),
                8.0,
                4.0,
                theme.accent,
            );
            let pct = format!("{:.0}%", fraction.clamp(0.0, 1.0) * 100.0);
            let pw = ui.measure(Face::Medium, 11.0, &pct);
            ui.text(Face::Medium, 11.0, x + w - 16.0 - pw, y + 38.0 - 1.0, theme.text_primary, &pct);
            return;
        }

        // Result banner (success or error).
        let mut list_top = top + 52.0;
        if let Some(result) = &outcome {
            let (ok, text) = match result {
                Ok(t) => (true, t.as_str()),
                Err(t) => (false, t.as_str()),
            };
            let lines = wrap(ui, text, w - 32.0, 11.5);
            let card_h = 40.0 + lines.len() as f32 * 16.0;
            ui.rounded(x, list_top, w, card_h, 12.0, theme.surface_dim);
            ui.border(x, list_top, w, card_h, 12.0, if ok { theme.accent } else { theme.danger });
            let title = if ok { "Installation complete" } else { "Installation failed" };
            ui.text(Face::SemiBold, 13.0, x + 16.0, list_top + 10.0, theme.text_primary, title);
            for (i, line) in lines.iter().enumerate() {
                ui.text(
                    Face::Regular,
                    11.5,
                    x + 16.0,
                    list_top + 32.0 + i as f32 * 16.0,
                    theme.text_muted,
                    line,
                );
            }
            targets.push(([x, list_top, w, card_h], Action::FabricDismiss));
            list_top += card_h + 10.0;
        }

        // Header row: step label, back / snapshots / retry buttons.
        let label = match self.step {
            Step::Game => "Choose a Minecraft version".to_owned(),
            Step::Loader => match &selected_game {
                Some(g) => format!("Fabric Loader for Minecraft {}", g.version),
                None => "Choose a Fabric Loader".to_owned(),
            },
        };
        let btn_w = 104.0;
        let label_w = (w - btn_w - 8.0).max(60.0);
        let label = ui.fit_text(Face::SemiBold, 13.5, &label, label_w);
        ui.text(Face::SemiBold, 13.5, x, list_top + 7.0, theme.text_primary, &label);
        let btn_rect = [x + w - btn_w, list_top, btn_w, 30.0];
        match self.step {
            Step::Game => button(
                ui,
                targets,
                btn_rect,
                if self.snapshots { "Snapshots: On" } else { "Snapshots: Off" },
                Action::FabricSnapshots,
                false,
            ),
            Step::Loader => button(ui, targets, btn_rect, "Back", Action::FabricBack, false),
        }

        let body_top = list_top + 40.0;
        let footer_h = if self.step == Step::Loader { 52.0 } else { 0.0 };
        let body_bottom = (ui.h - 12.0 - footer_h).max(body_top + ROW_H);

        // Empty / loading / error states.
        let loader_list: &[Loader] = match (&loaders, &selected_game) {
            (Some((v, l)), Some(g)) if *v == g.version => l,
            _ => &[],
        };
        let empty = match self.step {
            Step::Game => games.is_empty(),
            Step::Loader => loader_list.is_empty(),
        };
        if empty {
            let message = if loading {
                "Loading versions...".to_owned()
            } else if let Some(e) = &list_error {
                format!("{e} Tap Retry to try again.")
            } else if self.step == Step::Loader {
                "No Fabric Loader is available for this Minecraft version.".to_owned()
            } else {
                "No versions loaded.".to_owned()
            };
            for (i, line) in wrap(ui, &message, w, 12.0).iter().enumerate() {
                ui.text(Face::Regular, 12.0, x, body_top + i as f32 * 17.0, theme.text_muted, line);
            }
            if !loading && (list_error.is_some() || self.step == Step::Game) {
                let r = [x, body_top + 56.0, 110.0, 34.0];
                button(ui, targets, r, "Retry", Action::FabricRetry, true);
            }
            return;
        }

        // Scrollable list.
        let count = match self.step {
            Step::Game => games.len(),
            Step::Loader => loader_list.len(),
        };
        let max_scroll = (count as f32 * ROW_H - (body_bottom - body_top)).max(0.0);
        let scroll = scroll.clamp(0.0, max_scroll);
        self.view = [body_top, body_bottom, max_scroll];
        ui.set_clip(Some([x - 2.0, body_top, w + 4.0, (body_bottom - body_top).max(1.0)]));
        for i in 0..count {
            let y = body_top + i as f32 * ROW_H - scroll;
            if y + ROW_H < body_top || y > body_bottom {
                continue;
            }
            let (title, tag, selected, action) = match self.step {
                Step::Game => {
                    let g = &games[i];
                    (
                        g.version.clone(),
                        if g.stable { "Release" } else { "Snapshot" },
                        self.game == Some(i),
                        Action::FabricGame(i),
                    )
                }
                Step::Loader => {
                    let l = &loader_list[i];
                    let installed = selected_game
                        .as_ref()
                        .is_some_and(|g| profile_present(game_home, &g.version, &l.version));
                    (
                        l.version.clone(),
                        if installed {
                            "Installed"
                        } else if l.stable {
                            "Stable"
                        } else {
                            "Beta"
                        },
                        self.loader == Some(i),
                        Action::FabricLoader(i),
                    )
                }
            };
            let card_h = ROW_H - 6.0;
            ui.rounded(x, y, w, card_h, 10.0, if selected { theme.accent } else { theme.surface_dim });
            if !selected {
                ui.border(x, y, w, card_h, 10.0, theme.border);
            }
            let fg = if selected { theme.on_accent } else { theme.text_primary };
            let sub = if selected { theme.on_accent } else { theme.text_muted };
            ui.icon(Icon::Check, x + 12.0, y + (card_h - 16.0) * 0.5, 16.0, if selected { fg } else { theme.border });
            ui.text(Face::SemiBold, 13.5, x + 38.0, y + (card_h - 19.0) * 0.5, fg, &title);
            let tw = ui.measure(Face::Medium, 11.0, tag);
            ui.text(Face::Medium, 11.0, x + w - 14.0 - tw, y + (card_h - 15.0) * 0.5, sub, tag);
            let vis_y = y.max(body_top);
            let vis_h = (y + card_h).min(body_bottom) - vis_y;
            if vis_h > 4.0 {
                targets.push(([x, vis_y, w, vis_h], action));
            }
        }
        ui.set_clip(None);

        if self.step == Step::Loader {
            let ready = self.loader.is_some();
            let r = [x, body_bottom + 10.0, w, 38.0];
            if ready {
                button(ui, targets, r, "Install Fabric", Action::FabricInstall, true);
            } else {
                button(ui, targets, r, "Select a Loader version", Action::FabricInstall, false);
            }
        }
    }
}

fn wrap(ui: &Ui, text: &str, width: f32, size: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_owned()
        } else {
            format!("{line} {word}")
        };
        if !line.is_empty() && ui.measure(Face::Regular, size, &candidate) > width {
            lines.push(std::mem::take(&mut line));
            line = word.to_owned();
        } else {
            line = candidate;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn spawn(name: &str, job: impl FnOnce() + Send + 'static) {
    if std::thread::Builder::new()
        .name(name.into())
        .spawn(job)
        .is_err()
    {
        log::warn!("could not start worker thread {name}");
    }
}

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .https_only(true)
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .user_agent(USER_AGENT)
        .build()
}

fn describe_error(e: ureq::Error, what: &str) -> String {
    match e {
        ureq::Error::Status(404, _) => format!("{what}: not found (HTTP 404)."),
        ureq::Error::Status(429, _) => format!("{what}: too many requests, try again in a minute."),
        ureq::Error::Status(code, _) => format!("{what}: server returned HTTP {code}."),
        ureq::Error::Transport(t) => format!(
            "{what}: could not connect. Check your internet connection ({}).",
            t.kind()
        ),
    }
}

fn get_json(url: &str, what: &str) -> Result<Value, String> {
    let response = agent().get(url).call().map_err(|e| describe_error(e, what))?;
    let mut body = String::new();
    response
        .into_reader()
        .take(MAX_JSON)
        .read_to_string(&mut body)
        .map_err(|e| format!("{what}: download interrupted ({e})."))?;
    serde_json::from_str(&body).map_err(|_| format!("{what}: unexpected server response."))
}

fn fetch_games() -> Result<Vec<Game>, String> {
    let json = get_json(&format!("{META}/game"), "Minecraft version list")?;
    let games: Vec<Game> = json
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| {
                    Some(Game {
                        version: v["version"].as_str()?.to_owned(),
                        stable: v["stable"].as_bool().unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if games.is_empty() {
        return Err("Minecraft version list was empty.".into());
    }
    Ok(games)
}

fn load_loaders(shared: &Mutex<Shared>, game: String) {
    let result = fetch_loaders(&game);
    let mut s = lock(shared);
    s.loading = false;
    match result {
        Ok(list) => s.loaders = Some((game, list)),
        Err(e) => s.list_error = Some(e),
    }
}

fn fetch_loaders(game: &str) -> Result<Vec<Loader>, String> {
    let url = format!("{META}/loader/{}", encode(game));
    let json = get_json(&url, "Fabric Loader list")?;
    Ok(json
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| {
                    Some(Loader {
                        version: v["loader"]["version"].as_str()?.to_owned(),
                        stable: v["loader"]["stable"].as_bool().unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

/// Percent-encodes everything except unreserved URL characters.
fn encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

/// `net.fabricmc:fabric-loader:0.16.9` -> `net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar`
fn maven_path(name: &str) -> Option<String> {
    let mut parts = name.split(':');
    let group = parts.next()?;
    let artifact = parts.next()?;
    let version = parts.next()?;
    let classifier = parts.next();
    if group.is_empty() || artifact.is_empty() || version.is_empty() {
        return None;
    }
    let file = match classifier {
        Some(c) => format!("{artifact}-{version}-{c}.jar"),
        None => format!("{artifact}-{version}.jar"),
    };
    Some(format!("{}/{artifact}/{version}/{file}", group.replace('.', "/")))
}

/// Rejects names that could escape the target directory.
fn safe_component(text: &str) -> bool {
    !text.is_empty()
        && !text.contains(['/', '\\', ':', '\0'])
        && text != "."
        && text != ".."
}

fn profile_id(game: &str, loader: &str) -> String {
    format!("fabric-loader-{loader}-{game}")
}

fn profile_file(home: &Path, game: &str, loader: &str) -> PathBuf {
    let id = profile_id(game, loader);
    home.join("versions").join(&id).join(format!("{id}.json"))
}

fn profile_present(home: &Path, game: &str, loader: &str) -> bool {
    complete(home, &profile_id(game, loader))
}

/// True when `versions/<id>/<id>.json` exists and every library it lists is on disk.
fn complete(home: &Path, id: &str) -> bool {
    let file = home.join("versions").join(id).join(format!("{id}.json"));
    let Ok(text) = std::fs::read_to_string(file) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    json["libraries"].as_array().is_some_and(|libs| {
        libs.iter().all(|lib| {
            lib["name"]
                .as_str()
                .and_then(maven_path)
                .is_some_and(|p| std::fs::metadata(home.join("libraries").join(p)).is_ok_and(|m| m.len() > 0))
        })
    })
}

/// Fully installed Fabric profile ids in `home`, sorted.
pub(super) fn installed_profiles(home: &Path) -> Vec<String> {
    let mut ids: Vec<String> = std::fs::read_dir(home.join("versions"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|id| id.starts_with("fabric-loader-") && complete(home, id))
        .collect();
    ids.sort();
    ids
}

/// Short label for a profile id, e.g. `Fabric 0.16.9-1.21.4`.
pub(super) fn short_label(id: &str) -> String {
    format!("Fabric {}", id.strip_prefix("fabric-loader-").unwrap_or(id))
}

/// Title and subtitle for the version chooser, read from the profile's own JSON.
pub(super) fn describe(home: &Path, id: &str) -> (String, String) {
    let game = std::fs::read_to_string(home.join("versions").join(id).join(format!("{id}.json")))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|j| j["inheritsFrom"].as_str().map(str::to_owned));
    let rest = id.strip_prefix("fabric-loader-").unwrap_or(id);
    match game {
        Some(game) => {
            let loader = rest.strip_suffix(&format!("-{game}")).unwrap_or(rest);
            (format!("Fabric Loader {loader}"), format!("Minecraft {game}"))
        }
        None => (format!("Fabric {rest}"), String::new()),
    }
}

fn progress(shared: &Mutex<Shared>, stage: impl Into<String>, fraction: f32) {
    let mut s = lock(shared);
    s.stage = stage.into();
    s.fraction = fraction.clamp(0.0, 1.0);
}

fn io_error(action: &str, path: &Path, e: std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => {
            format!("Permission denied while {action} {}.", path.display())
        }
        _ if e.raw_os_error() == Some(28) => {
            "Not enough free storage space to finish the installation.".to_owned()
        }
        _ => format!("Could not {action} {} ({e}).", path.display()),
    }
}

/// Downloads `url` to `target` (via a `.part` file), reporting progress between
/// `from` and `to`. Verifies SHA-1 when given. Skips the transfer when `target`
/// already exists, is not empty and matches the hash (if any).
fn download(
    shared: &Mutex<Shared>,
    url: &str,
    target: &Path,
    sha1: Option<&str>,
    label: &str,
    (from, to): (f32, f32),
) -> Result<(), String> {
    if let Ok(meta) = std::fs::metadata(target)
        && meta.len() > 0
    {
        let intact = match sha1 {
            Some(expected) => std::fs::read(target)
                .map(|b| hex(&Sha1::digest(&b)).eq_ignore_ascii_case(expected))
                .unwrap_or(false),
            None => true,
        };
        if intact {
            progress(shared, format!("{label} (already downloaded)"), to);
            return Ok(());
        }
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_error("create folder", parent, e))?;
    }
    let part = target.with_extension("part");
    progress(shared, format!("Downloading {label}"), from);
    let response = agent().get(url).call().map_err(|e| describe_error(e, label))?;
    let total = response
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let mut reader = response.into_reader();
    let mut file = std::fs::File::create(&part).map_err(|e| io_error("write", &part, e))?;
    let mut hasher = Sha1::new();
    let mut buffer = [0u8; 32 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buffer).map_err(|e| {
            let _ = std::fs::remove_file(&part);
            format!("{label}: download interrupted, check your connection ({e}).")
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        file.write_all(&buffer[..n]).map_err(|e| {
            let _ = std::fs::remove_file(&part);
            io_error("write", &part, e)
        })?;
        done += n as u64;
        if total > 0 {
            progress(
                shared,
                format!("Downloading {label}"),
                from + (to - from) * (done as f32 / total as f32),
            );
        }
    }
    file.sync_all().map_err(|e| io_error("write", &part, e))?;
    drop(file);
    if done == 0 {
        let _ = std::fs::remove_file(&part);
        return Err(format!("{label}: the server sent an empty file."));
    }
    if let Some(expected) = sha1
        && !hex(&hasher.finalize()).eq_ignore_ascii_case(expected)
    {
        let _ = std::fs::remove_file(&part);
        return Err(format!("{label}: checksum mismatch, the download was corrupted."));
    }
    std::fs::rename(&part, target).map_err(|e| io_error("save", target, e))?;
    progress(shared, label.to_owned(), to);
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn run_install(shared: &Mutex<Shared>, home: &Path, game: &str, loader: &str) -> Result<String, String> {
    if !safe_component(game) || !safe_component(loader) {
        return Err("The selected version name is not valid.".into());
    }
    std::fs::create_dir_all(home).map_err(|e| io_error("create folder", home, e))?;

    // 1. Loader profile (versions/<id>/<id>.json).
    progress(shared, "Fetching Fabric profile", 0.02);
    let profile_url = format!(
        "{META}/loader/{}/{}/profile/json",
        encode(game),
        encode(loader)
    );
    let profile = get_json(&profile_url, "Fabric profile")?;
    let id = profile["id"].as_str().unwrap_or_default();
    if id != profile_id(game, loader) {
        return Err("Fabric returned a profile that does not match the selected versions.".into());
    }
    let libraries: Vec<Value> = profile["libraries"].as_array().cloned().unwrap_or_default();
    if libraries.is_empty() {
        return Err("The Fabric profile has no libraries.".into());
    }

    // 2. Libraries (libraries/<maven path>), 5%..80%.
    let count = libraries.len() as f32;
    for (i, lib) in libraries.iter().enumerate() {
        let name = lib["name"].as_str().unwrap_or_default();
        let path = maven_path(name).ok_or_else(|| format!("Unsupported library name: {name}"))?;
        let base = lib["url"].as_str().unwrap_or(MAVEN);
        let base = if base.ends_with('/') { base.to_owned() } else { format!("{base}/") };
        let target = home.join("libraries").join(&path);
        let from = 0.05 + 0.75 * (i as f32 / count);
        let to = 0.05 + 0.75 * ((i + 1) as f32 / count);
        let label = name.split(':').nth(1).unwrap_or(name).to_owned();
        download(
            shared,
            &format!("{base}{path}"),
            &target,
            lib["sha1"].as_str(),
            &label,
            (from, to),
        )?;
    }

    // 3. Write the profile last so an interrupted install is never reported as complete.
    let file = profile_file(home, game, loader);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_error("create folder", parent, e))?;
    }
    let text = serde_json::to_string_pretty(&profile).map_err(|e| e.to_string())?;
    std::fs::write(&file, text).map_err(|e| io_error("write", &file, e))?;

    // 4. Fabric API for the same Minecraft version, into mods/.
    progress(shared, "Looking up Fabric API", 0.82);
    let api = find_fabric_api(game)?;
    let mods = home.join("mods");
    if !safe_component(&api.filename) {
        return Err("Fabric API has an invalid file name.".into());
    }
    download(
        shared,
        &api.url,
        &mods.join(&api.filename),
        api.sha1.as_deref(),
        &format!("Fabric API {}", api.version),
        (0.84, 0.99),
    )?;

    progress(shared, "Done", 1.0);
    Ok(format!(
        "Fabric Loader {loader} for Minecraft {game} and Fabric API {} are installed in this profile.",
        api.version
    ))
}

struct ApiFile {
    version: String,
    filename: String,
    url: String,
    sha1: Option<String>,
}

fn find_fabric_api(game: &str) -> Result<ApiFile, String> {
    let url = format!(
        "{MODRINTH}?game_versions={}&loaders={}",
        encode(&format!("[\"{game}\"]")),
        encode("[\"fabric\"]")
    );
    let json = get_json(&url, "Fabric API lookup")?;
    pick_api(&json).ok_or_else(|| {
        format!("No Fabric API release exists for Minecraft {game}. Fabric Loader itself was installed.")
    })
}

/// Modrinth lists versions newest first; prefer the newest release, then any build.
fn pick_api(json: &Value) -> Option<ApiFile> {
    let list = json.as_array()?;
    let best = list
        .iter()
        .find(|v| v["version_type"].as_str() == Some("release"))
        .or_else(|| list.first())?;
    let files = best["files"].as_array()?;
    let file = files
        .iter()
        .find(|f| f["primary"].as_bool() == Some(true))
        .or_else(|| files.first())?;
    Some(ApiFile {
        version: best["version_number"].as_str()?.to_owned(),
        filename: file["filename"].as_str()?.to_owned(),
        url: file["url"].as_str()?.to_owned(),
        sha1: file["hashes"]["sha1"].as_str().map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maven_paths() {
        assert_eq!(
            maven_path("net.fabricmc:fabric-loader:0.16.9").as_deref(),
            Some("net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar")
        );
        assert_eq!(
            maven_path("org.ow2.asm:asm:9.7:sources").as_deref(),
            Some("org/ow2/asm/asm/9.7/asm-9.7-sources.jar")
        );
        assert_eq!(maven_path("broken"), None);
    }

    #[test]
    fn encodes_query_values() {
        assert_eq!(encode("[\"1.21.4\"]"), "%5B%221.21.4%22%5D");
        assert_eq!(encode("1.14 Pre-Release 1"), "1.14%20Pre-Release%201");
    }

    #[test]
    fn rejects_path_tricks() {
        assert!(safe_component("fabric-api-0.100.jar"));
        assert!(!safe_component("../evil.jar"));
        assert!(!safe_component("a/b"));
        assert!(!safe_component(".."));
    }

    #[test]
    fn profile_ids() {
        assert_eq!(profile_id("1.21.4", "0.16.9"), "fabric-loader-0.16.9-1.21.4");
    }

    #[test]
    fn picks_newest_release_and_primary_file() {
        let json: Value = serde_json::from_str(
            r#"[
              {"version_type":"beta","version_number":"9.9.9+beta","files":[]},
              {"version_type":"release","version_number":"0.119.4+1.21.4","files":[
                {"primary":false,"filename":"x-sources.jar","url":"https://cdn/x-sources.jar","hashes":{}},
                {"primary":true,"filename":"fabric-api-0.119.4+1.21.4.jar","url":"https://cdn/a.jar","hashes":{"sha1":"abc"}}
              ]}
            ]"#,
        )
        .unwrap();
        let api = pick_api(&json).unwrap();
        assert_eq!(api.version, "0.119.4+1.21.4");
        assert_eq!(api.filename, "fabric-api-0.119.4+1.21.4.jar");
        assert_eq!(api.sha1.as_deref(), Some("abc"));
        assert!(pick_api(&serde_json::json!([])).is_none());
    }
}
