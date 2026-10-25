//! The fallback stream function and adapters to `bake-ai` providers.
//!
//! Ported from Pi `packages/agent/src/stream-fn.ts` (v1.1.0): a host that
//! provides a default model runtime installs its stream function once, and
//! [`crate::Agent`] and the low-level loops use it when the caller passes
//! none. Pi keeps it in a module variable; here it is a process-wide lock.
//! [`registry_stream_fn`] is Bake's adapter from an [`ApiRegistry`] to a
//! [`StreamFn`].

use std::sync::{Arc, PoisonError, RwLock};

use bake_ai::ApiRegistry;

use crate::types::StreamFn;

static DEFAULT_STREAM_FN: RwLock<Option<StreamFn>> = RwLock::new(None);

/// The error when no stream function is passed or installed.
pub const NO_DEFAULT_STREAM_FN: &str =
    "No default stream function configured. Pass streamFn explicitly or call setDefaultStreamFn().";

/// Installs, or with `None` removes, the fallback stream function, Pi's
/// `setDefaultStreamFn`.
pub fn set_default_stream_fn(stream_fn: Option<StreamFn>) {
    *DEFAULT_STREAM_FN
        .write()
        .unwrap_or_else(PoisonError::into_inner) = stream_fn;
}

/// The fallback stream function, Pi's `getDefaultStreamFn`.
pub fn default_stream_fn() -> Result<StreamFn, String> {
    DEFAULT_STREAM_FN
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .ok_or_else(|| NO_DEFAULT_STREAM_FN.to_owned())
}

/// A stream function that dispatches on the model's `api` through
/// `registry`, as Pi's `Models.streamSimple` does. A model whose API has no
/// provider is an `Err`, Pi's thrown registry error.
pub fn registry_stream_fn(registry: Arc<ApiRegistry>) -> StreamFn {
    Arc::new(move |model, context, options| {
        registry
            .get(&model.api)
            .map(|provider| provider.stream_simple(model, context, options))
            .ok_or_else(|| bake_ai::RegistryError::NoProvider(model.api.clone()).to_string())
    })
}
