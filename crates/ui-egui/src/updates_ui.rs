//! One background check at start-up (and on demand from Help) for a newer release, and the modal
//! prompt that offers to update and restart.

use std::sync::mpsc::{Receiver, channel};

use crate::SlideApp;

type CheckResult = Result<Option<craft_update::Release>, String>;

#[derive(Default)]
pub(crate) struct Runtime {
    enabled: bool,
    startup_started: bool,
    manual: bool,
    check_rx: Option<Receiver<CheckResult>>,
    prompt: Option<craft_update::StartupPrompt>,
    update_rx: Option<Receiver<Result<(), String>>>,
}

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub(crate) fn enable(app: &mut SlideApp) {
    app.updates_rt.enabled = true;
}

/// Help > Check for Updates…: runs a check and reports the outcome either way.
pub(crate) fn request_check(app: &mut SlideApp) {
    if app.updates_rt.check_rx.is_none() {
        app.updates_rt.manual = true;
        start_check(app);
        app.notify("Checking for updates…");
    }
}

fn start_check(app: &mut SlideApp) {
    let (tx, rx) = channel();
    app.updates_rt.check_rx = Some(rx);
    std::thread::spawn(move || {
        let result = craft_update::Http::new("DeckCraft/Update").and_then(|http| craft_update::check_for_update(&craft_update::suite::DECKCRAFT, &http, VERSION));
        let _ = tx.send(result);
    });
}

pub(crate) fn tick(app: &mut SlideApp, ctx: &egui::Context) {
    if app.updates_rt.enabled && !app.updates_rt.startup_started {
        app.updates_rt.startup_started = true;
        start_check(app);
    }
    let Some(rx) = &app.updates_rt.check_rx else { return };
    match rx.try_recv() {
        Ok(result) => {
            app.updates_rt.check_rx = None;
            let manual = std::mem::take(&mut app.updates_rt.manual);
            match result {
                Ok(Some(release)) => app.updates_rt.prompt = Some(craft_update::StartupPrompt::new(&craft_update::suite::DECKCRAFT, release, VERSION)),
                Ok(None) if manual => app.notify(format!("DeckCraft {VERSION} is up to date.")),
                Err(e) if manual => app.notify(format!("Could not check for updates: {e}")),
                Ok(None) => {}
                Err(e) => log::warn!("DeckCraft update check failed: {e}"),
            }
            ctx.request_repaint();
        }
        Err(std::sync::mpsc::TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(200)),
        Err(std::sync::mpsc::TryRecvError::Disconnected) => app.updates_rt.check_rx = None,
    }
}

pub(crate) fn show(app: &mut SlideApp, ctx: &egui::Context) {
    let mut download = None;
    if let Some(prompt) = &mut app.updates_rt.prompt {
        if let Some(result) = app.updates_rt.update_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            app.updates_rt.update_rx = None;
            match result {
                // The guarded close path asks about unsaved work; the restart helper relaunches
                // once the process exits.
                Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(e) => {
                    prompt.status_message = Some(e);
                    prompt.is_installing = false;
                }
            }
        }
        match prompt.show(ctx) {
            craft_update::StartupPromptResponse::Pending => {}
            craft_update::StartupPromptResponse::UpdateAndRestart => {
                prompt.is_installing = true;
                if let Some(install) = craft_update::self_update::current(&craft_update::suite::DECKCRAFT) {
                    let release = prompt.release.clone();
                    let (tx, rx) = channel();
                    app.updates_rt.update_rx = Some(rx);
                    let ctx = ctx.clone();
                    std::thread::spawn(move || {
                        let run = || -> Result<(), String> {
                            let http = craft_update::Http::new("DeckCraft/Update").map_err(|e| format!("could not initialize HTTP: {e}"))?;
                            let paths = craft_update::Paths::for_app(&craft_update::suite::DECKCRAFT).map_err(|e| format!("could not find paths: {e}"))?;
                            let platform = craft_update::Platform::current().ok_or("unsupported platform for self update")?;
                            craft_update::self_update::update(
                                &craft_update::suite::DECKCRAFT,
                                &install,
                                &release,
                                platform,
                                &http,
                                &paths,
                                &craft_update::Stop::default(),
                                &mut |_| {},
                                &|| Ok(()),
                                Some(VERSION),
                            )
                            .map_err(|e| e.to_string())
                        };
                        let result = run();
                        if let Err(e) = &result {
                            log::warn!("DeckCraft self-update failed: {e}");
                        }
                        let _ = tx.send(result);
                        ctx.request_repaint();
                    });
                } else {
                    prompt.status_message = Some("Cannot auto-update this installation type. Please download manually.".to_string());
                    prompt.is_installing = false;
                }
            }
            craft_update::StartupPromptResponse::Download => {
                download = Some(prompt.release.html_url.clone());
                app.updates_rt.prompt = None;
            }
            craft_update::StartupPromptResponse::Later => app.updates_rt.prompt = None,
        }
    }
    if let Some(url) = download {
        ctx.open_url(egui::OpenUrl::new_tab(url));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_found_release_opens_the_prompt_and_later_dismisses_it() {
        let mut app = SlideApp::new(deckcraft_engine::Session::new(), crate::Services::default());
        let ctx = egui::Context::default();
        let (tx, rx) = channel();
        let release = craft_update::Release { tag: "v99.0.0".into(), ..Default::default() };
        assert!(tx.send(Ok(Some(release))).is_ok());
        app.updates_rt.check_rx = Some(rx);
        tick(&mut app, &ctx);
        assert_eq!(app.updates_rt.prompt.as_ref().map(|p| p.release.tag.as_str()), Some("v99.0.0"));
        app.updates_rt.prompt = None;
        ctx.run_ui(egui::RawInput::default(), |ui| show(&mut app, ui.ctx())).textures_delta.clear();
        assert!(app.updates_rt.prompt.is_none());
    }
}
