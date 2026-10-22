use super::config::Provider;
use std::{collections::BTreeMap, io::Write, path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, RwLock};

#[derive(Clone)]
pub struct ProviderRegistry {
    items: Arc<RwLock<BTreeMap<String, Provider>>>,
    update_lock: Arc<Mutex<()>>,
    path: Arc<PathBuf>,
}

#[derive(Debug)]
pub enum RegistryError {
    Invalid(String),
    NotFound,
    Conflict,
    Storage,
}

impl ProviderRegistry {
    pub fn load(path: PathBuf) -> Result<Self, String> {
        let providers: Vec<Provider> = match std::fs::read_to_string(&path) {
            Ok(text) => {
                serde_yaml::from_str(&text).map_err(|_| "供应商配置格式无效".to_string())?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(_) => return Err("无法读取供应商配置文件".into()),
        };
        let mut items = BTreeMap::new();
        for mut provider in providers {
            provider.validate()?;
            if items.insert(provider.id.clone(), provider).is_some() {
                return Err("供应商配置存在重复 id".into());
            }
        }
        Ok(Self {
            items: Arc::new(RwLock::new(items)),
            update_lock: Arc::new(Mutex::new(())),
            path: Arc::new(path),
        })
    }

    pub async fn list(&self) -> Vec<Provider> {
        self.items.read().await.values().cloned().collect()
    }

    pub async fn get(&self, id: &str) -> Option<Provider> {
        self.items.read().await.get(id).cloned()
    }

    pub async fn save(
        &self,
        mut provider: Provider,
        create: bool,
    ) -> Result<Provider, RegistryError> {
        provider.validate().map_err(RegistryError::Invalid)?;
        let _guard = self.update_lock.lock().await;
        let mut items = self.items.read().await.clone();
        match (create, items.contains_key(&provider.id)) {
            (true, true) => return Err(RegistryError::Conflict),
            (false, false) => return Err(RegistryError::NotFound),
            _ => {}
        }
        items.insert(provider.id.clone(), provider.clone());
        self.persist_and_apply(items).await?;
        Ok(provider)
    }

    pub async fn delete(&self, id: &str) -> Result<(), RegistryError> {
        let _guard = self.update_lock.lock().await;
        let mut items = self.items.read().await.clone();
        if items.remove(id).is_none() {
            return Err(RegistryError::NotFound);
        }
        self.persist_and_apply(items).await
    }

    async fn persist_and_apply(
        &self,
        items: BTreeMap<String, Provider>,
    ) -> Result<(), RegistryError> {
        let content = serde_yaml::to_string(&items.values().collect::<Vec<_>>())
            .map_err(|_| RegistryError::Storage)?;
        let path = self.path.clone();
        // A same-directory temporary file keeps failed writes from truncating the active config.
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(std::path::Path::new("."));
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            file.write_all(content.as_bytes())?;
            file.as_file().sync_all()?;
            file.persist(path.as_ref()).map_err(|e| e.error)?;
            Ok(())
        })
        .await
        .map_err(|_| RegistryError::Storage)?
        .map_err(|_| RegistryError::Storage)?;
        *self.items.write().await = items;
        Ok(())
    }
}
