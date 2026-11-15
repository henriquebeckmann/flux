//! Remote image cache for the UI: requests are fetched once on a background thread
//! (see `flux_platform::images` for the safety rules) and the windows are refreshed when done.

use std::{collections::HashMap, path::PathBuf};

use flux_api::ImageSource;
use gpui::{App, Global};

enum State {
    Loading,
    Ready(PathBuf),
    Failed(String),
}

pub struct Images {
    dir: PathBuf,
    entries: HashMap<String, State>,
}

impl Global for Images {}

pub fn install(cx: &mut App, cache_dir: PathBuf) {
    cx.set_global(Images { dir: cache_dir, entries: HashMap::new() });
}

impl Images {
    /// The local file for `url` once available; starts the download on first use.
    pub fn request(url: &str, cx: &mut App) -> Option<PathBuf> {
        let me = cx.global_mut::<Images>();
        match me.entries.get(url) {
            Some(State::Ready(p)) => return Some(p.clone()),
            Some(State::Loading | State::Failed(_)) => return None,
            None => {}
        }
        if let Some(p) = flux_platform::images::cached(url, &me.dir) {
            me.entries.insert(url.to_string(), State::Ready(p.clone()));
            return Some(p);
        }
        me.entries.insert(url.to_string(), State::Loading);
        let (dir, key) = (me.dir.clone(), url.to_string());
        let task = cx.background_executor().spawn({
            let key = key.clone();
            async move { flux_platform::images::fetch_blocking(&key, &dir) }
        });
        cx.spawn(async move |cx| {
            let result = task.await;
            let _ = cx.update(|cx| {
                let state = match result {
                    Ok(p) => State::Ready(p),
                    Err(e) => {
                        log::warn!("image {key}: {e}");
                        State::Failed(e)
                    }
                };
                cx.global_mut::<Images>().entries.insert(key, state);
                cx.refresh_windows();
            });
        })
        .detach();
        None
    }

    /// The image could not be fetched (so the fallback is shown for good).
    pub fn failed(source: &ImageSource, cx: &App) -> bool {
        match source {
            ImageSource::Url(u) => matches!(cx.global::<Images>().entries.get(u), Some(State::Failed(_))),
            ImageSource::Path(p) => !std::path::Path::new(p).is_file(),
        }
    }
}
