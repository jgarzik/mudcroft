//! Universe library loading
//!
//! A universe's config may name Lua libraries (`lib_hashes`: name -> code
//! hash). Every script that runs in the universe executes them first.

use crate::objects::ObjectStore;

/// Library sources for a universe as (name, code), sorted by name.
/// Missing or unreadable libraries are logged and skipped.
pub async fn universe_lib_codes(
    store: &ObjectStore,
    universe_id: &str,
) -> Result<Vec<(String, String)>, String> {
    let universe = match store.get_universe(universe_id).await {
        Ok(Some(u)) => u,
        Ok(None) => return Ok(Vec::new()), // No universe = no libs to load
        Err(e) => return Err(format!("Failed to get universe: {}", e)),
    };

    let Some(hashes_obj) = universe
        .config
        .get("lib_hashes")
        .and_then(|h| h.as_object())
    else {
        return Ok(Vec::new()); // No (or invalid) libs defined
    };

    // Load each library in order (sorted for determinism)
    let mut lib_names: Vec<_> = hashes_obj.keys().collect();
    lib_names.sort();

    let mut lib_codes = Vec::new();
    for lib_name in lib_names {
        let Some(hash) = hashes_obj.get(lib_name).and_then(|v| v.as_str()) else {
            continue;
        };
        match store.get_code(hash).await {
            Ok(Some(code)) => lib_codes.push((lib_name.clone(), code)),
            Ok(None) => tracing::warn!(
                "Library {} with hash {} not found in code_store",
                lib_name,
                hash
            ),
            Err(e) => tracing::warn!("Failed to load library {}: {}", lib_name, e),
        }
    }

    Ok(lib_codes)
}
