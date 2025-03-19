use kube::{Client, Api};
use k8s_openapi::api::apps::v1::StatefulSet;
use anyhow::Result;

pub async fn query_statefulset_replicas() -> Result<u64> {
    // Create a Kubernetes client
    let client = Client::try_default().await?;
    // Create an API handle for StatefulSets in the "default" namespace.
    let statefulsets: Api<StatefulSet> = Api::namespaced(client, "default");
    // Get the StatefulSet named "kv-store"
    let ss = statefulsets.get("kv-store").await?;
    // Retrieve the desired replica count from the spec; default to 0 if not present.
    let replicas: u64 = ss.spec
        .as_ref()
        .and_then(|spec| spec.replicas) // spec.replicas is an Option<i32>
        .map(|r| r as u64)
        .unwrap_or(0);
    
    Ok(replicas)
}
