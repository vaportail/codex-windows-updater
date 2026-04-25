// Suppress the console window on release builds — we are a GUI app.
// Debug builds keep the console so panics / eprintln! are visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cleanup;
mod config;
mod dark_window;
mod dialogs;
mod elevate;
mod extract;
mod installer;
mod junction;
mod launcher_update;
mod mode;
mod path_dialog;
mod proxy;
mod registry;
mod safety;
mod shortcut;
mod splash;
mod store;
mod uninstall;
mod updater;

use config::{Config, InstallMode};
use installer::{InstallMsg, InstallOptions};
use launcher_update::LauncherUpdateMsg;
use slint::ComponentHandle;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use store::Fetcher;
use updater::{DeferChoice, UpdateDecision};

slint::include_modules!();

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Smoke-test probe used by the self-update flow. MUST stay first — the
    // contract is that `--self-test` returns Ok(()) with zero side effects
    // (no cleanup, no mode detection, no UI, no network, no log writes).
    // The self-update worker spawns the freshly-downloaded `.new.exe` with
    // this flag to confirm the binary runs on this machine before swapping
    // it in.
    if args.iter().any(|a| a == "--self-test") {
        return Ok(());
    }

    // Best-effort cleanup of a half-written `codex-launcher.new.exe` from a
    // prior interrupted self-update. `codex-launcher.old.exe` is preserved
    // as the manual-rollback artifact.
    launcher_update::cleanup_stale_new_launcher();

    // Elevated re-spawn from the launcher self-update path. Skip mode
    // detection entirely — we just need to download/swap and exit.
    if let Some(target) = parse_string_flag(&args, "--auto-self-update") {
        return run_auto_self_update(&target);
    }

    // CLI fetcher override — takes precedence over updater.json for this run.
    let fetcher_override = parse_fetcher_flag(&args);
    let msix_path_override = parse_string_flag(&args, "--msix").map(std::path::PathBuf::from);

    if args.iter().any(|a| a == "--test-fetch") {
        return run_test_fetch();
    }
    if args.iter().any(|a| a == "--dump-sync") {
        return run_dump_sync();
    }
    if args.iter().any(|a| a == "--test-download") {
        return run_test_download(
            fetcher_override.unwrap_or_default(),
            msix_path_override.as_deref(),
        );
    }
    if args.iter().any(|a| a == "--test-extract") {
        return run_test_extract(
            msix_path_override.as_deref(),
            parse_string_flag(&args, "--version").as_deref(),
            parse_string_flag(&args, "--root").map(std::path::PathBuf::from),
            parse_string_flag(&args, "--keep").and_then(|s| s.parse::<u32>().ok()),
        );
    }
    if args.iter().any(|a| a == "--uninstall") {
        return run_uninstall_ui();
    }
    if args.iter().any(|a| a == "--debug-singleton") {
        return run_debug_singleton(parse_string_flag(&args, "--user-data-dir"));
    }

    // If this is an elevated re-spawn from the wizard, we skip mode
    // detection and run installer mode directly with pre-seeded state.
    let auto_install = parse_auto_install(&args);

    let m = mode::detect()?;

    match m {
        mode::Mode::Installer => {
            dark_window::install();
            let ui = AppWindow::new()?;
            prepare_window(&ui);
            wire_installer_ui(&ui, fetcher_override, auto_install)?;
            show_when_ready(&ui);
            slint::run_event_loop()?;
        }
        mode::Mode::Proxy(cfg) => {
            let auto_update = args.iter().any(|a| a == "--auto-update");
            return run_proxy(cfg, fetcher_override, &args, auto_update);
        }
    }
    Ok(())
}

/// Proxy-mode entry. Always shows a splash (screen 11) immediately so the
/// user sees *something*, then runs the update check on a background thread.
/// On Available we transition to screen 12 (the prompt); on any other outcome
/// we silent-launch Codex with forwarded args and quit the event loop.
fn run_proxy(
    cfg: Config,
    fetcher_override: Option<Fetcher>,
    args: &[String],
    auto_update: bool,
) -> anyhow::Result<()> {
    let root = mode::install_root()?;
    let effective_fetcher = fetcher_override.unwrap_or(cfg.fetcher);
    let mut cfg_for_check = cfg.clone();
    cfg_for_check.fetcher = effective_fetcher;

    // Forward everything we were invoked with — Start Menu shortcuts pass
    // nothing, file/protocol assoc passes a path/URL. Launcher-only flags
    // (--fetcher, --uninstall, ...) don't round-trip through proxy mode in
    // practice, so we don't bother filtering.
    let forward: Vec<String> = args.to_vec();

    // Elevated re-spawn from "Update now" on a System install. Skip the
    // check/prompt, open the window on screen 4, and run the update worker.
    if auto_update {
        // Defensive re-check after elevation: Codex may have been restarted
        // between the unelevated prompt and this re-spawn.
        if !prompt_kill_codex_for("updating") {
            // User aborted at the elevated prompt. Fall through to normal
            // proxy flow so the update banner is still shown.
            dark_window::install();
            let ui = AppWindow::new()?;
            prepare_window(&ui);
            wire_proxy_ui(
                &ui,
                cfg,
                fetcher_override,
                Some(UpdateDecision::UpToDate {
                    version: cfg_for_check.current_version.clone(),
                }),
                root,
                forward,
            )?;
            ui.set_current_screen(10);
            show_when_ready(&ui);
            slint::run_event_loop()?;
            return Ok(());
        }

        dark_window::install();
        let ui = AppWindow::new()?;
        prepare_window(&ui);
        // The unelevated bg check persisted launcher state then bumped the
        // shared cooldown — this re-spawn can't redo the check, so recover
        // the pending prompt from disk so on_request_launch can chain to
        // screen 30 after the update finishes.
        if let Some(updater::LauncherDecision::Available {
            current,
            latest,
            release_url,
        }) = updater::pending_launcher_from_state(&cfg)
        {
            ui.set_launcher_current_version(current.into());
            ui.set_launcher_latest_version(latest.into());
            ui.set_launcher_release_url(release_url.into());
        }
        let cfg_shared = wire_proxy_ui(&ui, cfg, fetcher_override, None, root.clone(), forward)?;
        ui.set_current_screen(4);
        ui.set_progress_phase("Starting update".into());
        ui.set_progress_detail("".into());
        ui.set_progress_indeterminate(true);
        spawn_update_worker(ui.as_weak(), cfg_shared);
        show_when_ready(&ui);
        slint::run_event_loop()?;
        return Ok(());
    }

    // Normal proxy path: native splash up immediately for instant feedback,
    // bg thread runs check, Slint window only built/shown when we have a
    // result that requires interaction (Available / launcher prompt). For
    // the silent-launch path the splash is dropped just before launch so
    // Codex appears unobstructed.
    let splash = splash::Splash::show(latest_codex_exe(&root, cfg.use_current_junction));

    dark_window::install();
    let ui = AppWindow::new()?;
    prepare_window(&ui);
    let cfg_for_launch = cfg.clone();
    // Bg thread writes `cfg_to_launch` into this shared Arc — without it,
    // a launcher-defer save would clobber the just-recorded Codex state.
    let cfg_shared = wire_proxy_ui(
        &ui,
        cfg,
        fetcher_override,
        None,
        root.clone(),
        forward.clone(),
    )?;

    let ui_weak = ui.as_weak();
    let cfg_shared_for_bg = cfg_shared.clone();
    std::thread::spawn(move || {
        if updater::auto_check_will_query(&cfg_for_check)
            || updater::launcher_auto_check_will_query(&cfg_for_check)
        {
            if let Some(splash) = &splash {
                splash.set_status("Checking for updates...");
            }
        }
        let codex_decision = updater::check_auto(&cfg_for_check, store::PRODUCT_ID_CODEX);
        // Run the launcher check against the same cfg snapshot so both
        // checks see the same `last_check_unix` and either both fire (when
        // cooldown elapsed) or both skip.
        let launcher_decision = updater::check_launcher_auto(&cfg_for_check);

        let cfg_to_launch =
            persist_runtime_state(&cfg_for_launch, &root, &codex_decision, &launcher_decision);
        // Sync into the shared Mutex so UI callbacks save from current state.
        *cfg_shared_for_bg.lock().unwrap() = cfg_to_launch.clone();

        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui_weak.upgrade() else { return };

            // Stash the launcher-update info on the UI regardless of what
            // we're about to display — the defer-Codex callback later
            // checks `launcher-release-url` to decide whether to chain into
            // screen 30 or just quit.
            if let updater::LauncherDecision::Available {
                current,
                latest,
                release_url,
            } = &launcher_decision
            {
                ui.set_launcher_current_version(current.clone().into());
                ui.set_launcher_latest_version(latest.clone().into());
                ui.set_launcher_release_url(release_url.clone().into());
            }

            match codex_decision {
                UpdateDecision::Available { current, latest } => {
                    // Codex prompt gates Codex spawn. Chaining into screen 30
                    // (when launcher update is also pending) happens in the
                    // on_request_update defer path, not here.
                    ui.set_update_current_version(current.into());
                    ui.set_update_latest_version(latest.into());
                    ui.set_current_screen(12);
                    drop(splash);
                    let _ = ui.show();
                }
                other => {
                    if let UpdateDecision::Error(e) = &other {
                        log_event(&format!("update check failed: {e}; launching anyway"));
                    }
                    drop(splash);
                    if let Err(e) = proxy::launch(&root, &cfg_to_launch, &forward) {
                        let msg = format!("launch failed: {e:#}");
                        log_event(&msg);
                        dialogs::error(&format!(
                            "Could not launch Codex.\n\n{msg}\n\nLog: {}",
                            launcher_log_display()
                        ));
                    }
                    // Codex spawned (or errored). If a launcher update is
                    // available, surface it now and keep the window open
                    // until the user dismisses. Otherwise quit.
                    if !ui.get_launcher_release_url().is_empty() {
                        ui.set_current_screen(30);
                        let _ = ui.show();
                    } else {
                        let _ = slint::quit_event_loop();
                    }
                }
            }
        });
    });

    // Slint event loop pumps the Slint window when shown; the splash runs
    // its own message loop on a dedicated thread until dropped.
    slint::run_event_loop()?;
    Ok(())
}

#[derive(Debug, Clone)]
struct AutoInstall {
    opts: InstallOptions,
}

fn parse_auto_install(args: &[String]) -> Option<AutoInstall> {
    if !args.iter().any(|a| a == "--auto-install") {
        return None;
    }
    let mode = match parse_string_flag(args, "--mode").as_deref() {
        Some("portable") => InstallMode::Portable,
        Some("system") => InstallMode::System,
        _ => InstallMode::User,
    };
    let root = parse_string_flag(args, "--path")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| installer::default_path(mode));
    let keep_versions = parse_string_flag(args, "--keep")
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);
    // Defaults follow mode: Portable opts out of system integration, others opt in.
    let portable = matches!(mode, InstallMode::Portable);
    let create_shortcut = if args.iter().any(|a| a == "--shortcut") {
        true
    } else if args.iter().any(|a| a == "--no-shortcut") {
        false
    } else {
        !portable
    };
    let register_uninstall = if args.iter().any(|a| a == "--register-uninstall") {
        true
    } else if args.iter().any(|a| a == "--no-register-uninstall") {
        false
    } else {
        !portable
    };
    let use_current_junction = !args.iter().any(|a| a == "--no-junction");
    let fetcher = parse_string_flag(args, "--fetcher")
        .and_then(|v| Fetcher::parse(&v))
        .unwrap_or_default();
    Some(AutoInstall {
        opts: InstallOptions {
            mode,
            root,
            create_shortcut,
            register_uninstall,
            keep_versions,
            fetcher,
            use_current_junction,
            local_msix: None,
        },
    })
}

/// Serialize an `InstallOptions` into CLI args suitable for `--auto-install`.
/// Used when the wizard needs to re-spawn itself elevated.
fn auto_install_args(opts: &InstallOptions) -> String {
    let mode = match opts.mode {
        InstallMode::Portable => "portable",
        InstallMode::User => "user",
        InstallMode::System => "system",
    };
    let fetcher = match opts.fetcher {
        Fetcher::Winget => "winget",
        _ => "direct",
    };
    let mut s = format!(
        "--auto-install --mode {} --path \"{}\" --keep {} --fetcher {}",
        mode,
        opts.root.display(),
        opts.keep_versions,
        fetcher,
    );
    // Pass explicit flags so the elevated re-spawn doesn't fall back to
    // mode-based defaults (which would silently flip Portable settings).
    s.push_str(if opts.create_shortcut {
        " --shortcut"
    } else {
        " --no-shortcut"
    });
    s.push_str(if opts.register_uninstall {
        " --register-uninstall"
    } else {
        " --no-register-uninstall"
    });
    if !opts.use_current_junction {
        s.push_str(" --no-junction");
    }
    s
}

fn wire_installer_ui(
    ui: &AppWindow,
    fetcher_override: Option<Fetcher>,
    auto: Option<AutoInstall>,
) -> anyhow::Result<()> {
    // Seed defaults.
    let default_mode = auto
        .as_ref()
        .map(|a| a.opts.mode)
        .unwrap_or(InstallMode::User);
    ui.set_current_screen(0);
    ui.set_install_mode(install_mode_to_int(default_mode));
    ui.set_install_path(
        auto.as_ref()
            .map(|a| a.opts.root.clone())
            .unwrap_or_else(|| installer::default_path(default_mode))
            .to_string_lossy()
            .into_owned()
            .into(),
    );
    ui.set_keep_versions(
        auto.as_ref()
            .map(|a| a.opts.keep_versions as i32)
            .unwrap_or(2),
    );
    let portable_default = matches!(default_mode, InstallMode::Portable);
    ui.set_create_shortcut(
        auto.as_ref()
            .map(|a| a.opts.create_shortcut)
            .unwrap_or(!portable_default),
    );
    ui.set_register_uninstall(
        auto.as_ref()
            .map(|a| a.opts.register_uninstall)
            .unwrap_or(!portable_default),
    );
    ui.set_use_current_junction(
        auto.as_ref()
            .map(|a| a.opts.use_current_junction)
            .unwrap_or(true),
    );
    ui.set_fetcher(fetcher_to_int(
        auto.as_ref()
            .map(|a| a.opts.fetcher)
            .or(fetcher_override)
            .unwrap_or_default(),
    ));

    // Mode change: reset path and the system-integration toggles to that
    // mode's defaults. Portable opts out of shortcut + uninstaller entry;
    // User/System opt in.
    {
        let ui_weak = ui.as_weak();
        ui.on_mode_selected(move |m| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mode = int_to_install_mode(m);
            let portable = matches!(mode, InstallMode::Portable);
            ui.set_install_path(
                installer::default_path(mode)
                    .to_string_lossy()
                    .into_owned()
                    .into(),
            );
            ui.set_create_shortcut(!portable);
            ui.set_register_uninstall(!portable);
        });
    }

    // Browse → native folder picker (IFileOpenDialog, FOS_PICKFOLDERS).
    {
        let ui_weak = ui.as_weak();
        ui.on_path_browse(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let initial = std::path::PathBuf::from(ui.get_install_path().to_string());
            match path_dialog::pick_folder() {
                Ok(Some(path)) => {
                    ui.set_install_path(path.to_string_lossy().into_owned().into());
                }
                Ok(None) => {} // user cancelled
                Err(e) => eprintln!("folder picker failed: {e:#}"),
            }
            let _ = initial;
        });
    }

    // Quit / Close
    {
        let ui_weak = ui.as_weak();
        ui.on_request_quit(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let _ = ui.window().hide();
            }
        });
    }

    // Launch → spawn Codex via proxy::launch (self-heals the junction,
    // skips if already running, forwards no args since this is fresh install).
    {
        let ui_weak = ui.as_weak();
        ui.on_request_launch(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let root = std::path::PathBuf::from(ui.get_install_path().to_string());
            let use_junction = ui.get_use_current_junction();
            // Build a minimal Config for proxy::launch — only the junction
            // flag is consulted. Remaining fields are defaulted.
            let cfg = Config {
                install_mode: int_to_install_mode(ui.get_install_mode()),
                current_version: ui.get_installed_version().to_string(),
                update_policy: Default::default(),
                last_check_unix: None,
                suppress_until_unix: None,
                known_latest: None,
                skipped_version: None,
                keep_versions: ui.get_keep_versions() as u32,
                fetcher: int_to_fetcher(ui.get_fetcher()),
                use_current_junction: use_junction,
                register_uninstall: ui.get_register_uninstall(),
                known_latest_launcher: None,
                skipped_launcher_version: None,
                launcher_suppress_until_unix: None,
            };
            if let Err(e) = proxy::launch(&root, &cfg, &[]) {
                let msg = format!("post-install launch failed: {e:#}");
                log_event(&msg);
                dialogs::error(&format!(
                    "Could not launch Codex.\n\n{msg}\n\nLog: {}",
                    launcher_log_display()
                ));
            }
            let _ = ui.window().hide();
        });
    }

    // Install → UAC gate for System mode, then spawn worker thread.
    {
        let ui_weak = ui.as_weak();
        ui.on_request_install(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let opts = InstallOptions {
                mode: int_to_install_mode(ui.get_install_mode()),
                root: std::path::PathBuf::from(ui.get_install_path().to_string()),
                create_shortcut: ui.get_create_shortcut(),
                register_uninstall: ui.get_register_uninstall(),
                keep_versions: ui.get_keep_versions() as u32,
                fetcher: int_to_fetcher(ui.get_fetcher()),
                use_current_junction: ui.get_use_current_junction(),
                local_msix: None,
            };

            // Program Files / HKLM writes need admin. Re-spawn elevated
            // with --auto-install and exit this (unelevated) wizard.
            if matches!(opts.mode, InstallMode::System) && !elevate::is_elevated() {
                match elevate::respawn_elevated(&auto_install_args(&opts)) {
                    Ok(()) => {
                        let _ = ui.window().hide();
                        return;
                    }
                    Err(e) => {
                        ui.set_error_text(format!("Couldn't obtain admin rights: {e:#}").into());
                        ui.set_current_screen(6);
                        return;
                    }
                }
            }

            start_install(ui.as_weak(), opts);
        });
    }

    // If re-spawned via --auto-install, jump straight to progress and kick
    // the worker without user interaction.
    if let Some(auto) = auto {
        ui.set_current_screen(4);
        ui.set_progress_phase("Starting installation".into());
        ui.set_progress_indeterminate(true);
        start_install(ui.as_weak(), auto.opts);
    }

    Ok(())
}

/// Uninstall entry called from main's `--uninstall` short-circuit. Handles
/// UAC self-elevation, then opens the Slint window on the confirm screen.
/// The worker thread is only spawned once the user clicks "Uninstall".
fn run_uninstall_ui() -> anyhow::Result<()> {
    let ctx = match uninstall::load_context() {
        Ok(c) => c,
        Err(e) => {
            dialogs::error(&format!(
                "Couldn't read install state: {e:#}\n\n\
                 This launcher doesn't appear to be a valid Codex install. \
                 No action taken."
            ));
            return Ok(());
        }
    };

    // HKLM / Program Files removal needs admin. Self-elevate silently before
    // showing any UI — the unelevated process exits so the user only sees
    // the UAC prompt followed by the real uninstall window.
    if uninstall::need_elevation(&ctx) {
        elevate::respawn_elevated("--uninstall")?;
        return Ok(());
    }

    dark_window::install();
    let ui = AppWindow::new()?;
    prepare_window(&ui);
    wire_uninstall_ui(&ui, ctx)?;
    ui.set_current_screen(20);
    show_when_ready(&ui);
    slint::run_event_loop()?;
    Ok(())
}

fn wire_uninstall_ui(ui: &AppWindow, ctx: uninstall::UninstallContext) -> anyhow::Result<()> {
    use std::sync::Mutex;

    // Context gets consumed by the worker thread on confirm; wrap so the
    // closure can take it.
    let ctx_holder = std::sync::Arc::new(Mutex::new(Some(ctx)));

    {
        let ui_weak = ui.as_weak();
        ui.on_request_quit(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let _ = ui.window().hide();
            }
            let _ = slint::quit_event_loop();
        });
    }

    {
        let ui_weak = ui.as_weak();
        let ctx_holder = ctx_holder.clone();
        ui.on_request_uninstall_start(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let Some(ctx) = ctx_holder.lock().unwrap().take() else {
                // Button mashed twice — worker already running.
                return;
            };
            ui.set_current_screen(21);
            ui.set_progress_phase("Starting".into());
            ui.set_progress_detail("".into());
            ui.set_progress_indeterminate(true);
            let ui_weak_inner = ui_weak.clone();
            std::thread::spawn(move || {
                uninstall::run_worker(ctx, move |msg| {
                    let weak = ui_weak_inner.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        let Some(ui) = weak.upgrade() else { return };
                        apply_uninstall_msg(&ui, msg);
                    });
                });
            });
        });
    }

    Ok(())
}

fn apply_uninstall_msg(ui: &AppWindow, msg: uninstall::UninstallMsg) {
    match msg {
        uninstall::UninstallMsg::Phase { phase, detail } => {
            ui.set_progress_phase(phase.into());
            ui.set_progress_detail(detail.into());
            ui.set_progress_indeterminate(true);
        }
        uninstall::UninstallMsg::Progress(Some(f)) => {
            ui.set_progress_indeterminate(false);
            ui.set_progress_fraction(f);
        }
        uninstall::UninstallMsg::Progress(None) => {
            ui.set_progress_indeterminate(true);
        }
        uninstall::UninstallMsg::Done { log_path } => {
            ui.set_uninstall_log_path(log_path.into());
            ui.set_current_screen(22);
        }
        uninstall::UninstallMsg::Error(e) => {
            ui.set_error_text(e.into());
            ui.set_current_screen(23);
        }
    }
}

fn start_install(ui_weak: slint::Weak<AppWindow>, opts: InstallOptions) {
    std::thread::spawn(move || {
        installer::run(opts, move |msg| {
            let weak = ui_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = weak.upgrade() else { return };
                apply_install_msg(&ui, msg);
            });
        });
    });
}

fn wire_proxy_ui(
    ui: &AppWindow,
    cfg: Config,
    fetcher_override: Option<Fetcher>,
    initial_decision: Option<UpdateDecision>,
    root: std::path::PathBuf,
    forward_args: Vec<String>,
) -> anyhow::Result<Arc<Mutex<Config>>> {
    // Seed proxy status screen (shown as return screen after "not now"/snooze
    // and as the anchor for the "Check for updates" button).
    let effective_fetcher = fetcher_override.unwrap_or(cfg.fetcher);
    ui.set_proxy_status(
        format!(
            "Installed version: {}\nFetcher: {:?}",
            cfg.current_version, effective_fetcher,
        )
        .into(),
    );
    ui.set_update_current_version(cfg.current_version.clone().into());

    // Shared cfg for all callbacks. Each callback locks briefly.
    let cfg = Arc::new(Mutex::new(cfg));
    let root = Arc::new(root);
    let forward_args = Arc::new(forward_args);
    // Tracks whether we're still in the proxy-startup "launch-intent" phase.
    // When true, deferring an update prompt should launch the currently
    // installed Codex and exit (the user asked to launch Codex, not open the
    // launcher UI). When false — e.g. after an explicit "Check for updates"
    // — deferring just returns to the proxy status screen.
    let pending_launch = Arc::new(AtomicBool::new(true));

    // If caller handed us an Available decision, jump straight to the prompt.
    // Otherwise caller is responsible for setting the initial screen (e.g.
    // the --auto-update path jumps to screen 4 itself).
    if let Some(UpdateDecision::Available { current, latest }) = initial_decision {
        ui.set_update_current_version(current.into());
        ui.set_update_latest_version(latest.into());
        ui.set_current_screen(12);
    }

    // Explicit "Check for updates" button on the proxy status screen.
    {
        let ui_weak = ui.as_weak();
        let cfg = cfg.clone();
        let root = root.clone();
        let pending_launch = pending_launch.clone();
        ui.on_request_check_updates(move || {
            // Explicit user action — no longer a launch-intent flow.
            pending_launch.store(false, Ordering::SeqCst);
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_current_screen(11);
            }
            spawn_force_check(ui_weak.clone(), cfg.clone(), (*root).clone());
        });
    }

    // Update dialog buttons (defer choices + UpdateNow).
    {
        let ui_weak = ui.as_weak();
        let cfg = cfg.clone();
        let root = root.clone();
        let forward_args = forward_args.clone();
        let pending_launch = pending_launch.clone();
        ui.on_request_update(move |choice_idx| {
            let choice = int_to_defer_choice(choice_idx);
            let Some(ui) = ui_weak.upgrade() else { return };

            if choice == DeferChoice::UpdateNow {
                // Codex must not be running during update — its file handles
                // on versions/<oldver>/Codex.exe etc. prevent clean junction
                // swap and the running instance wouldn't pick up the new
                // version anyway. Prompt before doing anything destructive.
                if !prompt_kill_codex_for("updating") {
                    ui.set_current_screen(10);
                    return;
                }

                // System-install updates write to Program Files + HKLM —
                // need admin. Re-spawn ourselves elevated with --auto-update
                // (proxy mode re-enters and jumps straight to the worker).
                let install_mode = cfg.lock().unwrap().install_mode;
                if matches!(install_mode, InstallMode::System) && !elevate::is_elevated() {
                    match elevate::respawn_elevated("--auto-update") {
                        Ok(()) => {
                            let _ = ui.window().hide();
                            return;
                        }
                        Err(e) => {
                            ui.set_update_error_text(
                                format!("Couldn't obtain admin rights: {e:#}").into(),
                            );
                            ui.set_current_screen(10);
                            return;
                        }
                    }
                }

                // Transition to progress screen and kick the update worker.
                ui.set_current_screen(4);
                ui.set_progress_phase("Starting update".into());
                ui.set_progress_detail("".into());
                ui.set_progress_indeterminate(true);
                spawn_update_worker(ui_weak.clone(), cfg.clone());
                return;
            }

            // Defer path — persist the choice.
            let latest = ui.get_update_latest_version().to_string();
            let cfg_snapshot = {
                let mut c = cfg.lock().unwrap();
                updater::apply_defer(&mut c, choice, &latest);
                let _ = c.save_runtime(&root);
                c.clone()
            };

            // If the user reached this prompt via the proxy-startup
            // launch-intent flow, fulfill the original intent: launch the
            // currently installed Codex. Then either chain into the
            // launcher-update prompt (if one was pending) or exit.
            // Otherwise (explicit "Check for updates"), fall back to the
            // proxy status screen.
            if pending_launch.swap(false, Ordering::SeqCst) {
                if let Err(e) = proxy::launch(&root, &cfg_snapshot, &forward_args) {
                    eprintln!("launch failed: {e:#}");
                }
                if !ui.get_launcher_release_url().is_empty() {
                    ui.set_current_screen(30);
                } else {
                    let _ = ui.window().hide();
                    let _ = slint::quit_event_loop();
                }
                return;
            }
            ui.set_current_screen(10);
        });
    }

    // Launcher self-update prompt callback (screen 30).
    {
        let ui_weak = ui.as_weak();
        let cfg = cfg.clone();
        let root = root.clone();
        ui.on_request_launcher_action(move |action_idx| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let action = int_to_launcher_choice(action_idx);

            if action == updater::LauncherDeferChoice::ViewRelease {
                let url = ui.get_launcher_release_url().to_string();
                if !url.is_empty() {
                    open_url(&url);
                }
                // Don't dismiss; user may also pick a defer option after.
                return;
            }

            if action == updater::LauncherDeferChoice::ApplyUpdate {
                let latest = ui.get_launcher_latest_version().to_string();
                if latest.is_empty() {
                    return;
                }
                // System install lives under Program Files — replacing the
                // exe needs admin. Re-spawn elevated and exit; the elevated
                // process re-enters main, takes the --auto-self-update
                // branch, and runs the worker UI.
                let install_mode = cfg.lock().unwrap().install_mode;
                if matches!(install_mode, InstallMode::System) && !elevate::is_elevated() {
                    let cli = format!("--auto-self-update {latest}");
                    match elevate::respawn_elevated(&cli) {
                        Ok(()) => {
                            let _ = ui.window().hide();
                            let _ = slint::quit_event_loop();
                            return;
                        }
                        Err(e) => {
                            ui.set_error_text(
                                format!("Couldn't obtain admin rights: {e:#}").into(),
                            );
                            ui.set_current_screen(6);
                            return;
                        }
                    }
                }

                ui.set_current_screen(4);
                ui.set_progress_phase("Starting".into());
                ui.set_progress_detail("".into());
                ui.set_progress_indeterminate(true);
                spawn_launcher_self_update_worker(ui_weak.clone(), latest);
                return;
            }

            let latest = ui.get_launcher_latest_version().to_string();
            {
                let mut c = cfg.lock().unwrap();
                updater::apply_launcher_defer(&mut c, action, &latest);
                let _ = c.save_runtime(&root);
            }

            let _ = ui.window().hide();
            let _ = slint::quit_event_loop();
        });
    }

    // Quit / Close / Launch.
    {
        let ui_weak = ui.as_weak();
        ui.on_request_quit(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let _ = ui.window().hide();
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let cfg = cfg.clone();
        let root = root.clone();
        let forward_args = forward_args.clone();
        ui.on_request_launch(move || {
            let cfg_snapshot = cfg.lock().unwrap().clone();
            if let Err(e) = proxy::launch(&root, &cfg_snapshot, &forward_args) {
                eprintln!("launch failed: {e:#}");
            }
            if let Some(ui) = ui_weak.upgrade() {
                // Surface pending launcher prompt before exit — otherwise
                // user wouldn't see it again until next cooldown.
                if !ui.get_launcher_release_url().is_empty() {
                    ui.set_current_screen(30);
                } else {
                    let _ = ui.window().hide();
                }
            }
        });
    }

    Ok(cfg)
}

fn spawn_force_check(
    ui_weak: slint::Weak<AppWindow>,
    cfg: Arc<Mutex<Config>>,
    root: std::path::PathBuf,
) {
    std::thread::spawn(move || {
        let snapshot = cfg.lock().unwrap().clone();
        let decision = updater::check_now(&snapshot, store::PRODUCT_ID_CODEX);
        apply_update_decision(ui_weak, cfg, &root, decision);
    });
}

fn apply_update_decision(
    ui_weak: slint::Weak<AppWindow>,
    cfg: Arc<Mutex<Config>>,
    install_root: &std::path::Path,
    decision: UpdateDecision,
) {
    // Persist last_check / known_latest for UpToDate + Available.
    match &decision {
        UpdateDecision::UpToDate { version }
        | UpdateDecision::Available {
            latest: version, ..
        } => {
            let mut c = cfg.lock().unwrap();
            updater::record_check(&mut c, version);
            let _ = c.save_runtime(install_root);
        }
        _ => {}
    }

    let _ = slint::invoke_from_event_loop(move || {
        let Some(ui) = ui_weak.upgrade() else { return };
        match decision {
            UpdateDecision::Skipped { reason } => {
                eprintln!("update check skipped: {reason}");
                ui.set_current_screen(10);
            }
            UpdateDecision::UpToDate { version } => {
                ui.set_update_current_version(version.into());
                ui.set_current_screen(13);
            }
            UpdateDecision::Available { current, latest } => {
                ui.set_update_current_version(current.into());
                ui.set_update_latest_version(latest.into());
                ui.set_current_screen(12);
            }
            UpdateDecision::Error(e) => {
                ui.set_update_error_text(e.clone().into());
                eprintln!("update check failed: {e}");
                // Fall through to proxy status — user can retry manually.
                ui.set_current_screen(10);
            }
        }
    });
}

/// Run the elevated launcher self-update flow. Skips proxy/installer logic;
/// just opens the AppWindow on the progress screen and runs the worker.
/// The user closes the window after Done — at which point either this
/// elevated process or the unelevated one (User/Portable path) is the only
/// one running, and on next launch the updated launcher takes over.
fn run_auto_self_update(target_version: &str) -> anyhow::Result<()> {
    dark_window::install();
    let ui = AppWindow::new()?;
    prepare_window(&ui);

    // Surface the target version on screen 31's success message.
    ui.set_launcher_latest_version(target_version.into());
    ui.set_current_screen(4);
    ui.set_progress_phase("Starting".into());
    ui.set_progress_detail("".into());
    ui.set_progress_indeterminate(true);

    {
        let ui_weak = ui.as_weak();
        ui.on_request_quit(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let _ = ui.window().hide();
            }
            let _ = slint::quit_event_loop();
        });
    }

    spawn_launcher_self_update_worker(ui.as_weak(), target_version.to_string());
    show_when_ready(&ui);
    slint::run_event_loop()?;
    Ok(())
}

fn spawn_launcher_self_update_worker(ui_weak: slint::Weak<AppWindow>, target_version: String) {
    std::thread::spawn(move || {
        launcher_update::apply(&target_version, move |msg| {
            let weak = ui_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    apply_launcher_update_msg(&ui, msg);
                }
            });
        });
    });
}

fn apply_launcher_update_msg(ui: &AppWindow, msg: LauncherUpdateMsg) {
    match msg {
        LauncherUpdateMsg::Phase { phase, detail } => {
            ui.set_progress_phase(phase.into());
            ui.set_progress_detail(detail.into());
        }
        LauncherUpdateMsg::Progress(Some(f)) => {
            ui.set_progress_indeterminate(false);
            ui.set_progress_fraction(f);
        }
        LauncherUpdateMsg::Progress(None) => {
            ui.set_progress_indeterminate(true);
        }
        LauncherUpdateMsg::Done => {
            ui.set_current_screen(31);
        }
        LauncherUpdateMsg::Error(e) => {
            ui.set_error_text(e.into());
            ui.set_current_screen(6);
        }
    }
}

fn spawn_update_worker(ui_weak: slint::Weak<AppWindow>, cfg: Arc<Mutex<Config>>) {
    let root = match mode::install_root() {
        Ok(r) => r,
        Err(e) => {
            let msg = InstallMsg::Error(format!("{:#}", e));
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_weak.upgrade() {
                    apply_install_msg(&ui, msg);
                }
            });
            return;
        }
    };

    std::thread::spawn(move || {
        installer::update(root, move |msg| {
            // On Done, refresh the in-memory config so later screens see the
            // new current_version.
            if let InstallMsg::Done { version } = &msg {
                if let Ok(mut c) = cfg.lock() {
                    c.current_version = version.clone();
                }
            }
            let weak = ui_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    apply_install_msg(&ui, msg);
                }
            });
        });
    });
}

/// Center a freshly-created Slint window on the primary monitor.
///
/// Slint's default placement tends to land in the top-left corner on Windows,
/// which looks unfinished. We compute center from `GetSystemMetrics(SM_CX/CYSCREEN)`
/// scaled by `GetDpiForSystem()` since our AppWindow is declared in logical
/// pixels (580x420) and the screen metrics come back in physical.
/// Pre-show window setup: center and hide. Dark client area + dark title
/// bar are handled before the window paints by the CBT hook from
/// `dark_window::install`, which each AppWindow entry point invokes
/// just before constructing the window.
fn prepare_window(ui: &AppWindow) {
    use windows::Win32::UI::HiDpi::GetDpiForSystem;
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

    const LOGICAL_W: f32 = 580.0;
    const LOGICAL_H: f32 = 420.0;

    unsafe {
        let screen_w = GetSystemMetrics(SM_CXSCREEN);
        let screen_h = GetSystemMetrics(SM_CYSCREEN);
        let scale = GetDpiForSystem() as f32 / 96.0;
        let win_w = (LOGICAL_W * scale) as i32;
        let win_h = (LOGICAL_H * scale) as i32;
        let x = ((screen_w - win_w) / 2).max(0);
        let y = ((screen_h - win_h) / 2).max(0);
        ui.window().set_position(slint::PhysicalPosition::new(x, y));
    }
    let _ = ui.window().hide();
}

/// Schedule `ui.show()` once the event loop runs, so Slint's renderer has
/// time to produce a first frame before the window becomes visible.
fn show_when_ready(ui: &AppWindow) {
    let weak = ui.as_weak();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = weak.upgrade() {
            let _ = ui.show();
        }
    });
}

/// If any `Codex.exe` processes are running, prompt the user to terminate
/// them. Returns true if it's safe to proceed (nothing was running, or user
/// confirmed termination and all PIDs exited). Returns false if the user
/// cancelled, or if termination failed — caller should abort the destructive
/// operation.
///
/// `action` is the verb used in the prompt, e.g. "updating" / "uninstalling".
fn prompt_kill_codex_for(action: &str) -> bool {
    let pids = proxy::find_codex_pids();
    if pids.is_empty() {
        return true;
    }
    let msg = format!(
        "Codex is currently running ({} process{}). It must be closed before \
         {action}.\n\n\
         Terminate it and continue?\n\n\
         Click No to cancel. No files have been modified yet.",
        pids.len(),
        if pids.len() == 1 { "" } else { "es" },
    );
    if !dialogs::yes_no("Codex is running", &msg) {
        return false;
    }
    proxy::terminate_pids(&pids, 5000);
    let still = proxy::find_codex_pids();
    if !still.is_empty() {
        dialogs::error(&format!(
            "Failed to terminate {} Codex process(es). Aborting.",
            still.len()
        ));
        return false;
    }
    true
}

fn int_to_defer_choice(i: i32) -> DeferChoice {
    match i {
        0 => DeferChoice::UpdateNow,
        1 => DeferChoice::NotNow,
        2 => DeferChoice::SkipThisVersion,
        3 => DeferChoice::SnoozeOneDay,
        4 => DeferChoice::SnoozeSevenDays,
        5 => DeferChoice::Never,
        _ => DeferChoice::NotNow,
    }
}

fn int_to_launcher_choice(i: i32) -> updater::LauncherDeferChoice {
    use updater::LauncherDeferChoice as L;
    match i {
        0 => L::ViewRelease,
        1 => L::NotNow,
        2 => L::SkipThisVersion,
        3 => L::SnoozeOneDay,
        4 => L::SnoozeSevenDays,
        5 => L::Never,
        6 => L::ApplyUpdate,
        _ => L::NotNow,
    }
}

/// Open `url` in the user's default browser via `ShellExecuteW`. Best-effort;
/// failures (typically nothing registered for http) are swallowed.
fn open_url(url: &str) {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let verb: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    let url_w: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let _ = ShellExecuteW(
            HWND::default(),
            PCWSTR(verb.as_ptr()),
            PCWSTR(url_w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// Apply both check decisions to a fresh Config snapshot and persist it.
/// Only a successful Codex check bumps `last_check_unix` — that timestamp
/// gates the shared cooldown, so a launcher-only success must not advance
/// it (would suppress Codex retry after a Store failure). Returns the
/// updated Config; caller syncs it into the shared `Arc<Mutex<Config>>`.
fn persist_runtime_state(
    base: &Config,
    install_root: &std::path::Path,
    codex: &UpdateDecision,
    launcher: &updater::LauncherDecision,
) -> Config {
    let mut c = base.clone();
    let mut changed = false;
    match codex {
        UpdateDecision::UpToDate { version }
        | UpdateDecision::Available {
            latest: version, ..
        } => {
            updater::record_check(&mut c, version);
            changed = true;
        }
        _ => {}
    }
    match launcher {
        updater::LauncherDecision::UpToDate { version }
        | updater::LauncherDecision::Available {
            latest: version, ..
        } => {
            c.known_latest_launcher = Some(version.clone());
            // Do NOT bump last_check_unix — see fn doc. GitHub call is
            // cheap; happy to re-run it alongside the next Codex retry.
            changed = true;
        }
        _ => {}
    }
    if changed {
        let _ = c.save_runtime(install_root);
    }
    c
}

fn apply_install_msg(ui: &AppWindow, msg: InstallMsg) {
    match msg {
        InstallMsg::Phase { phase, detail } => {
            ui.set_progress_phase(phase.into());
            ui.set_progress_detail(detail.into());
        }
        InstallMsg::Progress(Some(f)) => {
            ui.set_progress_indeterminate(false);
            ui.set_progress_fraction(f);
        }
        InstallMsg::Progress(None) => {
            ui.set_progress_indeterminate(true);
        }
        InstallMsg::Done { version } => {
            ui.set_installed_version(version.into());
            ui.set_current_screen(5);
        }
        InstallMsg::Error(e) => {
            ui.set_error_text(e.into());
            ui.set_current_screen(6);
        }
    }
}

fn install_mode_to_int(m: InstallMode) -> i32 {
    match m {
        InstallMode::Portable => 0,
        InstallMode::User => 1,
        InstallMode::System => 2,
    }
}

fn int_to_install_mode(i: i32) -> InstallMode {
    match i {
        0 => InstallMode::Portable,
        2 => InstallMode::System,
        _ => InstallMode::User,
    }
}

fn fetcher_to_int(f: Fetcher) -> i32 {
    match f {
        Fetcher::Direct => 0,
        Fetcher::Winget => 1,
        Fetcher::LocalFile => 0, // not representable in the combobox; fall back
    }
}

fn int_to_fetcher(i: i32) -> Fetcher {
    match i {
        1 => Fetcher::Winget,
        _ => Fetcher::Direct,
    }
}

/// Resolve the Codex.exe to launch.
///
/// When `use_junction` is true: scan for the newest numeric-version dir,
/// verify the junction points at it (self-heal via remove+recreate if not),
/// and return the junction path (`versions/current/Codex.exe`). Launching
/// via the stable junction path is what lets user-applied AV exclusions
/// survive updates.
///
/// When `use_junction` is false, or the junction can't be established,
/// return the newest numeric-version `Codex.exe` directly.
fn latest_codex_exe(root: &std::path::Path, use_junction: bool) -> Option<std::path::PathBuf> {
    let versions = root.join("versions");
    let (newest_name, newest_exe) = newest_numeric_version(&versions)?;

    if !use_junction {
        return Some(newest_exe);
    }

    let link = versions.join("current");
    let expected_target = versions.join(&newest_name);

    // Check where the junction currently points. If it's stale (or missing),
    // re-point it at the newest version. Non-fatal on failure — we'll just
    // launch via the numeric path.
    let stale = match std::fs::canonicalize(&link) {
        Ok(actual) => std::fs::canonicalize(&expected_target)
            .map(|want| actual != want)
            .unwrap_or(true),
        Err(_) => true, // missing / broken
    };
    if stale {
        if let Err(e) = junction::set_current(root, &newest_name) {
            eprintln!("warn: couldn't repair versions/current junction: {e:#}");
            return Some(newest_exe);
        }
    }

    let via_junction = link.join("Codex.exe");
    if via_junction.exists() {
        Some(via_junction)
    } else {
        Some(newest_exe)
    }
}

/// Scan `versions/` for the highest numeric-dotted dir containing `Codex.exe`.
/// Returns `(dir_name, full_path_to_Codex.exe)`.
fn newest_numeric_version(versions: &std::path::Path) -> Option<(String, std::path::PathBuf)> {
    let mut best: Option<(Vec<u64>, String, std::path::PathBuf)> = None;
    for entry in std::fs::read_dir(versions).ok()? {
        let entry = entry.ok()?;
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".partial") || name == "current" {
            continue;
        }
        if entry.file_type().map(|t| t.is_symlink()).unwrap_or(false) {
            continue;
        }
        let parts: Vec<u64> = name.split('.').map(|p| p.parse().unwrap_or(0)).collect();
        let codex = entry.path().join("Codex.exe");
        if !codex.exists() {
            continue;
        }
        match &best {
            None => best = Some((parts, name, codex)),
            Some((cur, _, _)) if parts > *cur => best = Some((parts, name, codex)),
            _ => {}
        }
    }
    best.map(|(_, n, p)| (n, p))
}

/// Fallback string used in user-facing messages when `LOCALAPPDATA` is
/// unexpectedly unset. Mirrors the resolved path's shape so users can
/// still find the directory if needed.
const LAUNCHER_LOG_PATH_FALLBACK: &str = "%LOCALAPPDATA%\\codex-launcher\\launcher.log";

/// Per-user log location, always writable regardless of install mode
/// (System installs to Program Files can't write next to the launcher exe
/// without elevation). Also kept *outside* the install root so the
/// uninstaller's `rmdir(root)` doesn't trip on a leftover log file.
fn launcher_log_path() -> Option<std::path::PathBuf> {
    let base = std::env::var("LOCALAPPDATA").ok()?;
    Some(
        std::path::PathBuf::from(base)
            .join("codex-launcher")
            .join("launcher.log"),
    )
}

/// User-facing rendering of the log path. Returns the resolved path if
/// available, the fallback string otherwise. Use this when constructing
/// MessageBox / dialog text — never inline `launcher_log_path()` + a
/// fallback literal at the call site.
fn launcher_log_display() -> String {
    launcher_log_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| LAUNCHER_LOG_PATH_FALLBACK.to_string())
}

/// Append a single timestamped line to the per-user launcher log. Used to
/// surface errors / events from the GUI subsystem build (where `eprintln!`
/// is a no-op). Best-effort; failures are swallowed.
fn log_event(msg: &str) {
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};
    let Some(path) = launcher_log_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!("[{ts}] {msg}\n");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Parse `--fetcher <direct|winget|local>`. Returns None if absent or unrecognized.
fn parse_fetcher_flag(args: &[String]) -> Option<Fetcher> {
    parse_string_flag(args, "--fetcher").and_then(|v| Fetcher::parse(&v))
}

/// Parse `--name value` or `--name=value`. Returns the value as String.
fn parse_string_flag(args: &[String], name: &str) -> Option<String> {
    let eq_prefix = format!("{name}=");
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == name {
            return it.next().cloned();
        }
        if let Some(v) = a.strip_prefix(&eq_prefix) {
            return Some(v.to_string());
        }
    }
    None
}

// -- debug / smoke-test entrypoints -----------------------------------------

/// Run the same singleton probe the production launcher uses, against the
/// userData path Codex would compute (or one given via `--user-data-dir`),
/// and report the result. Useful for diagnosing "why doesn't my Codex
/// launch the way I expect" without rebuilding any logic.
fn run_debug_singleton(user_data_dir: Option<String>) -> anyhow::Result<()> {
    let udd = match user_data_dir.map(std::path::PathBuf::from) {
        Some(p) => p,
        None => proxy::codex_user_data_dir().ok_or_else(|| {
            anyhow::anyhow!("could not derive Codex userData path (set APPDATA?)")
        })?,
    };
    println!("Probing Codex singleton with userData: {}", udd.display());

    match proxy::find_singleton_holder(&udd) {
        Some(holder) => {
            println!("Singleton is HELD.");
            println!("  PID:        {}", holder.pid);
            println!("  Image path: {}", holder.image_path.display());
        }
        None => {
            println!("Singleton is NOT held — no responsive Codex main process found.");
            println!("(Spawning would create a fresh main; orphan child processes do not");
            println!(" count because their parent's message pump is dead.)");
        }
    }
    Ok(())
}

fn run_test_fetch() -> anyhow::Result<()> {
    println!("Dumping SyncUpdates via Direct fetcher...");
    let xml = store::debug_dump_sync_xml(store::PRODUCT_ID_CODEX)?;
    println!("SyncUpdates response length: {} bytes", xml.len());
    Ok(())
}

fn run_dump_sync() -> anyhow::Result<()> {
    let xml = store::debug_dump_sync_xml(store::PRODUCT_ID_CODEX)?;
    std::fs::write("sync_dump.xml", &xml)?;
    eprintln!("wrote sync_dump.xml ({} bytes)", xml.len());
    Ok(())
}

fn run_test_download(fetcher: Fetcher, msix_path: Option<&std::path::Path>) -> anyhow::Result<()> {
    let dest = std::path::PathBuf::from("test_download");
    std::fs::create_dir_all(&dest)?;
    println!("Downloading latest Codex MSIX via {:?}...", fetcher);
    let mut last_logged = 0u64;
    let mut progress = |done: u64, total: Option<u64>| {
        if done - last_logged >= 5 * 1024 * 1024 || total.map(|t| done == t).unwrap_or(false) {
            match total {
                Some(t) => println!(
                    "  {} / {} bytes ({:.1}%)",
                    done,
                    t,
                    (done as f64 / t as f64) * 100.0
                ),
                None => println!("  {} bytes", done),
            }
            last_logged = done;
        }
    };
    let result = match fetcher {
        Fetcher::LocalFile => {
            let path = msix_path.ok_or_else(|| {
                anyhow::anyhow!("--fetcher local requires --msix <path/to/file.msix>")
            })?;
            store::local_file::from_file(path, &dest, &mut progress)?
        }
        _ => store::download_latest(fetcher, store::PRODUCT_ID_CODEX, &dest, &mut progress)?,
    };
    println!("\n  moniker : {}", result.moniker);
    println!("  version : {}", result.version);
    println!("  file    : {}", result.msix_path.display());
    Ok(())
}

fn run_test_extract(
    msix_path: Option<&std::path::Path>,
    version_override: Option<&str>,
    root_override: Option<std::path::PathBuf>,
    keep_override: Option<u32>,
) -> anyhow::Result<()> {
    let msix = msix_path
        .ok_or_else(|| anyhow::anyhow!("--test-extract requires --msix <path/to/file.msix>"))?;
    let root = root_override.unwrap_or_else(|| std::path::PathBuf::from("test_install"));
    std::fs::create_dir_all(&root)?;

    let version = match version_override {
        Some(v) => v.to_string(),
        None => msix
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.split('_').nth(1))
            .ok_or_else(|| {
                anyhow::anyhow!("couldn't parse version from filename; pass --version <x.y.z.w>")
            })?
            .to_string(),
    };

    println!(
        "Extracting {} -> {} (version {})",
        msix.display(),
        root.display(),
        version
    );
    let mut last_logged = 0u64;
    let mut progress = |done: u64, total: Option<u64>| {
        let step = total.map(|t| (t / 20).max(1)).unwrap_or(50);
        if done - last_logged >= step || total.map(|t| done == t).unwrap_or(false) {
            match total {
                Some(t) => println!("  {done}/{t} entries"),
                None => println!("  {done} entries"),
            }
            last_logged = done;
        }
    };
    let out = extract::extract_app(msix, &root, &version, &mut progress)?;
    println!("extracted to {}", out.display());

    let keep = keep_override.unwrap_or(2);
    let removed = extract::prune_versions(&root, keep)?;
    if removed.is_empty() {
        println!("prune: nothing to remove (keep={keep})");
    } else {
        println!("prune: removed {removed:?} (keep={keep})");
    }
    Ok(())
}
