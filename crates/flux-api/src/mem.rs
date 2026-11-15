//! In-memory implementations, useful for tests and headless runs.

use std::{collections::HashMap, sync::Mutex};

use crate::{BoxFuture, SecretError, SecretStore};

/// A `SecretStore` that keeps everything in RAM (never persisted).
#[derive(Default)]
pub struct MemSecretStore(Mutex<HashMap<(String, String), String>>);

impl SecretStore for MemSecretStore {
    fn get<'a>(&'a self, ext: &'a str, key: &'a str) -> BoxFuture<'a, Result<Option<String>, SecretError>> {
        Box::pin(async move { Ok(self.0.lock().unwrap().get(&(ext.into(), key.into())).cloned()) })
    }
    fn set<'a>(&'a self, ext: &'a str, key: &'a str, value: &'a str) -> BoxFuture<'a, Result<(), SecretError>> {
        Box::pin(async move {
            self.0.lock().unwrap().insert((ext.into(), key.into()), value.into());
            Ok(())
        })
    }
    fn remove<'a>(&'a self, ext: &'a str, key: &'a str) -> BoxFuture<'a, Result<(), SecretError>> {
        Box::pin(async move {
            self.0.lock().unwrap().remove(&(ext.into(), key.into()));
            Ok(())
        })
    }
}

/// A `SecretStore` that always fails, mimicking a machine without Secret Service.
pub struct UnavailableSecretStore(pub String);

impl SecretStore for UnavailableSecretStore {
    fn get<'a>(&'a self, _: &'a str, _: &'a str) -> BoxFuture<'a, Result<Option<String>, SecretError>> {
        Box::pin(async move { Err(SecretError(self.0.clone())) })
    }
    fn set<'a>(&'a self, _: &'a str, _: &'a str, _: &'a str) -> BoxFuture<'a, Result<(), SecretError>> {
        Box::pin(async move { Err(SecretError(self.0.clone())) })
    }
    fn remove<'a>(&'a self, _: &'a str, _: &'a str) -> BoxFuture<'a, Result<(), SecretError>> {
        Box::pin(async move { Err(SecretError(self.0.clone())) })
    }
}
