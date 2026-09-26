//! The inventory from Kubernetes: a reflector over the labelled ConfigMaps
//! in one namespace. The snapshot is rebuilt from the cache on each read.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::StreamExt;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::runtime::reflector::{self, Store};
use kube::runtime::{WatchStreamExt, watcher};
use kube::{Api, Client, ResourceExt};
use tracing::{info, warn};

use crate::inventory::{INVENTORY_LABEL, Inventory, Snapshot, from_configmaps};

pub struct KubeInventory {
    store: Store<ConfigMap>,
    ready: Arc<AtomicBool>,
}

impl KubeInventory {
    pub fn start(client: Client, namespace: &str) -> Arc<Self> {
        let api: Api<ConfigMap> = Api::namespaced(client, namespace);
        let config = watcher::Config::default().labels(&format!("{INVENTORY_LABEL}=true"));
        let (store, writer) = reflector::store();
        let ready = Arc::new(AtomicBool::new(false));

        tokio::spawn(
            watcher(api, config)
                .default_backoff()
                .reflect(writer)
                .applied_objects()
                .for_each(|event| async move {
                    match event {
                        Ok(cm) => info!(configmap = %cm.name_any(), "inventory ConfigMap changed"),
                        Err(e) => warn!(error = %e, "watching inventory ConfigMaps"),
                    }
                }),
        );
        {
            let store = store.clone();
            let ready = ready.clone();
            tokio::spawn(async move {
                // Re-poll with a timeout: the readiness signal only wakes the
                // most recent waiter.
                loop {
                    match tokio::time::timeout(Duration::from_secs(1), store.wait_until_ready())
                        .await
                    {
                        Ok(Ok(())) => {
                            info!("inventory cache synced");
                            ready.store(true, Ordering::Relaxed);
                            return;
                        }
                        Ok(Err(_)) => return,
                        Err(_) => continue,
                    }
                }
            });
        }
        Arc::new(KubeInventory { store, ready })
    }
}

impl Inventory for KubeInventory {
    fn snapshot(&self) -> Snapshot {
        let maps = self.store.state();
        from_configmaps(
            maps.iter()
                .filter_map(|cm| Some((cm.metadata.name.as_deref()?, cm.data.as_ref()?))),
        )
    }

    fn ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }
}
