//! Primary setup boundary: no Engine preparation may precede GUI collection admission.
use std::{path::PathBuf, sync::Arc};
use tune_core::{
    engine::{Engine, Host},
    gui_collection::ensure_gui_collection_owner,
};

pub(crate) fn open_primary_engine(config: PathBuf, data: PathBuf, host: Arc<dyn Host>) -> Result<Arc<Engine>, String> {
    ensure_gui_collection_owner().map_err(|e| e.to_string())?;
    Engine::open(config, data, host)
}
