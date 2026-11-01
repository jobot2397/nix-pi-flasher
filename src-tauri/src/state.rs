use std::sync::Mutex;

use tauri::{AppHandle, Manager};

use crate::search::SearchAuth;

pub struct AppState {
    pub http: reqwest::Client,
    pub inner: Mutex<Inner>,
}

pub struct Inner {
    pub build_running: bool,
    pub flash_running: bool,
    pub cancel_requested: bool,
    pub build_pid: Option<u32>,
    pub last_image: Option<String>,
    pub allowed_disks: Vec<String>,
    pub search: Option<SearchAuth>,
}

impl AppState {
    pub fn new() -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .user_agent("nix-pi-flasher")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;
        Ok(Self {
            http,
            inner: Mutex::new(Inner::default()),
        })
    }
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            build_running: false,
            flash_running: false,
            cancel_requested: false,
            build_pid: None,
            last_image: None,
            allowed_disks: Vec::new(),
            search: None,
        }
    }
}

pub(crate) fn lock(mutex: &Mutex<Inner>) -> std::sync::MutexGuard<'_, Inner> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn with_inner<R>(app: &AppHandle, update: impl FnOnce(&mut Inner) -> R) -> R {
    let state = app.state::<AppState>();
    let mut inner = lock(&state.inner);
    update(&mut inner)
}
