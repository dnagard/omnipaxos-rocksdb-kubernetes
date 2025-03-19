# OmniPaxos KV-Store on Kubernetes

This project implements a **distributed key–value store** running on Kubernetes. It uses **OmniPaxos** for consensus, ensuring that every change is applied uniformly across all server nodes. The system leverages Kubernetes StatefulSets for managing kv-store pods, along with persistent storage via RocksDB and PersistentVolumeClaims, to support automatic fail recovery and dynamic scaling (reconfiguration) of the cluster.

It was implemented as the final project for the Distributed Systems Advanced Course at KTH (ID2203) VT25.  
Developed by [Daniel Nagard](https://github.com/dnagard) and [Batuhan Yıldırım](https://github.com/Batuhanyldirim).

---

## Table of Contents

- [General Information](#general-information)
  - [Project Description](#project-description)
  - [Technologies & Tools](#technologies--tools)
- [Architecture & Implementation](#architecture--implementation)
  - [System Overview](#system-overview)
  - [OmniPaxos Usage](#omnipaxos-usage)
  - [Persistent Storage](#persistent-storage)
  - [Automatic Reconfiguration](#automatic-reconfiguration)
  - [Failure Recovery](#failure-recovery)
- [How to Use](#how-to-use)
  - [Running the Cluster](#running-the-cluster)
  - [Client Interaction](#client-interaction)
  - [Demo Scenario](#demo-scenario)
- [Deployment & Demo Details](#deployment--demo-details)
  - [Setup & Installation](#setup--installation)
  - [Docker Image Details](#docker-image-details)
  - [Troubleshooting](#troubleshooting)
- [Repository Structure](#repository-structure)
- [Future Work / Possible Improvements](#future-work--possible-improvements)
- [Acknowledgements & References](#acknowledgements--references)

---

## General Information

### Project Description

The core purpose of this project is to implement a distributed key–value store that runs on Kubernetes. It uses OmniPaxos to achieve consensus among servers so that all changes are logged and applied consistently. The system automatically recovers from node failures and supports dynamic scaling. For example, if you scale the StatefulSet up (using `kubectl scale statefulset kv-store --replicas=4`), the leader node detects the change via the Kubernetes API and triggers a reconfiguration. The new node receives the latest state (via a state catchup message containing all key–value pairs) and is added to a fresh OmniPaxos instance. Persistent storage (using RocksDB and Kubernetes PersistentVolumeClaims) ensures that state is not lost on node crashes.

### Technologies & Tools

- **Programming Language:** Rust  
- **Consensus Algorithm:** OmniPaxos (based on SequencePaxos)  
- **Storage:** RocksDB and Kubernetes Persistent Volumes  
- **Containerization & Orchestration:** Docker, Kubernetes, Minikube  
- **Kubernetes Features:** StatefulSets, RBAC (to allow kv-store pods to query the Kubernetes API), PersistentVolumeClaims  
- **Additional Libraries:** OmniPaxos Rust bindings, Tokio (for async runtime)

---

## Architecture & Implementation

### System Overview

The system comprises two types of container images:

- **kv_store:**  
  Runs the distributed key–value server. It handles storage, OmniPaxos consensus, Kubernetes API querying (for reconfiguration), and state catchup.  
- **net_actor:**  
  Acts as a central multiplexer for intra-cluster communication and as the user interface via a CLI. It routes messages between nodes through TCP connections (using a headless service defined in the Kubernetes configuration).

### OmniPaxos Usage

OmniPaxos is used to build a distributed log of server commands. Every change to the key–value store is appended to this log. The consensus mechanism guarantees that all nodes apply the same commands in the same order, thereby ensuring data consistency.  
For more details, please refer to [Omni-Paxos: Breaking the Barriers of Partial Connectivity](https://dl.acm.org/doi/pdf/10.1145/3552326.3587441) and the [OmniPaxos website](https://omnipaxos.com/).

### Persistent Storage

Persistent storage is twofold in this project:
- **Kubernetes Persistent Volumes:**  
  Ensure that pod data (e.g., the actual key–value pairs) is preserved across pod restarts.
- **RocksDB:**  
  Used by OmniPaxos to store its metadata and log entries, allowing recovery of consensus state upon restart.

### Automatic Reconfiguration

The kv_store nodes periodically query the Kubernetes API (with appropriate RBAC permissions) to detect changes in the StatefulSet’s replica count. When the number of pods increases, the leader node triggers a reconfiguration:
- A StopSign is proposed via OmniPaxos.
- Upon commitment of the StopSign, the leader collects the current state (all key–value pairs) and sends it to the new node using a `StateCatchup` message.
- A new OmniPaxos instance is started on all servers with fresh persistent storage (its log resets to zero), ensuring that new decisions can be recorded safely.

*Note:* Scaling down is not fully supported yet. Removing nodes can lead to blocking behavior in the central sender, which is an area for future improvement.

### Failure Recovery

When a kv_store node crashes:
- Kubernetes automatically restarts it.
- The recovering node loads its state from persistent storage (via RocksDB).
- It then contacts the leader to receive any missing consensus decisions using a snapshot (or state catchup) mechanism.
- The leader sends a snapshot of the current state to ensure the recovering node is synchronized with the cluster.

---

## How to Use

### Running the Cluster

1. **Start Minikube (or your Kubernetes cluster):**

   ```bash
   minikube start --driver=docker
   ```

2. **Check cluster status:**

   ```bash
   minikube status
   kubectl get nodes
   ```

3. **Deploy the Pods:**

   Apply the Kubernetes configuration:

   ```bash
   kubectl apply -f kube.yml
   ```

4. **Check the Pods:**

   ```bash
   kubectl get pods
   ```

   Example output:

   ```bash
   NAME         READY   STATUS    RESTARTS   AGE
   kv-store-0   1/1     Running   0          86s
   kv-store-1   1/1     Running   0          80s
   kv-store-2   1/1     Running   0          78s
   net          1/1     Running   0          86s
   ```

5. **Attach to the network actor to interact with the cluster:**

   ```bash
   kubectl attach -it net
   ```

### Client Interaction

The network actor CLI supports basic commands:

- **Put:**  
  ```bash
  put <key> <value> [optional:port]
  ```
  Example: `put a 1`  
  This writes `{ key: "a", value: "1" }` to the store.  
  Optionally specify a port (8001, 8002, etc.) to direct the request to a specific node. If omitted, the request goes to the leader.

- **Get:**  
  ```bash
  get <key> [optional:port]
  ```
  Example: `get a 8001`  
  Retrieves the value for key "a" from the node listening on port 8001.

- **Delete:**  
  ```bash
  delete <key> [optional:port]
  ```
  Example: `delete a`  
  Removes the key "a" from the store.

The CLI prints connection events, responses (such as the latest decision index), and error messages (e.g., if a connection is dropped).

### Demo Scenario

1. **Put Data:**  
   Type:  
   ```
   put a 1
   put b 2
   put c 3
   ```
   This inserts key `"a"` with value `"1"`, key `b` with value `2`, and key `c` with value `3`.

2. **Get Data:**  
   Retrieve the value on different nodes:  
   ```
   get a 8001
   get b 8002
   get c 8003
   ```

3. **Delete Data:**  
   Remove key `"a"`:  
   ```
   delete a
   ```

4. **Scaling Up:**  
   In a separate terminal, scale up the StatefulSet:  
   ```bash
   kubectl scale statefulset kv-store --replicas=4
   ```
   Then, in the network actor CLI, issue a `get` command specifying port 8004 (or the port corresponding to the new node) to verify that the new node has received the state.

5. **Failure Recovery:**  
   - Insert a few values using the `put` command.  
   - Force delete a pod (e.g., `kubectl delete pod kv-store-1 --force`) to simulate a crash.  
   - Watch the network actor output; the recovering node should rejoin the cluster and catch up on missed decisions.
   - To test if the system handles catching up on missed decisions, time a `put` command with the downtime caused by the pod deletion. 
   - Can view the Snapshot recovery and decision log with `kubectl logs -f kv-store-1`

---

## Deployment & Demo Details

### Setup & Installation

- **Minikube** is used for local testing.
- Ensure Docker is installed.
- The project uses environment variables and configuration provided via `kube.yml`.
- When building images, use the provided Dockerfiles in each folder (for kv_store and net_actor). You can also use the provided docker-compose file for local testing.

### Docker Image Details

- **Prebuilt Images:**  
  - `dnagard/kv_store:pStoreRec`  
    Use this image for kv_store pods. It includes persistent storage and reconfiguration features.
  - `dnagard/net_actor:latest`  
    Use this image for the network actor.
- These prebuilt, multiplatform images are particularly useful for running on Apple Silicon where build times are very long. You can build from source if desired, but these images offer a quick start.
- **To test with in-memory storage, use the build on the [persistentDeactivated](https://github.com/dnagard/omnipaxos-rocksdb-kubernetes/tree/persistentDeactivated) branch. This branch does not have scaling implemented, however.**. 

### Troubleshooting

- **Invalid Commands:**  
  If the get/put/delete command is misformatted, the network actor may panic. In such cases, restart the cluster using:
  ```bash
  minikube delete && minikube start --driver=docker && kubectl apply -f kube.yml
  ```
- **Scaling Down Issues:**  
  Currently, scaling down (removing nodes) may cause blocking behavior. If a node is removed, the system may continue trying to send messages to it. A restart of the system may be necessary until scaling-down support is improved.

---

## Repository Structure

```
├── kv_store
│   ├── Cargo.lock
│   ├── Cargo.toml
│   ├── Dockerfile
│   └── src
│       ├── database.rs
│       ├── kubernetes.rs
│       ├── kv.rs
│       ├── main.rs
│       ├── network.rs
│       └── server.rs
├── network_actor
│   ├── Cargo.lock
│   ├── Cargo.toml
│   ├── Dockerfile
│   └── src
│       ├── main.rs
│       └── network.rs
|
├── docker-compose.yml
├── kube.yml
└── README.md
```

---

## Future Work / Possible Improvements

- **Scaling Down Support:**  
  Improve handling of node removal and prevent blocking when a pod is deleted.
- **Robust Network Actor:**  
  Enhance the network actor to better handle transient failures and reduce blocking on dead nodes.
- **State Transfer Efficiency:**  
  Optimize the state catchup mechanism to reduce the number of messages required for synchronizing state between nodes.
- **Client Resilience:**  
  Implement better error handling and recovery for client interactions in the network actor.

---

## Acknowledgements & References

- **OmniPaxos:**  
  Developed by Harold Ng at KTH.  
  - [OmniPaxos: Breaking the Barriers of Partial Connectivity](https://dl.acm.org/doi/pdf/10.1145/3552326.3587441)  
  - [OmniPaxos – A Distributed Log Library](https://omnipaxos.com/)
- **Rust Documentation:**  
  [Rust Book](https://doc.rust-lang.org/book/)
- **Kubernetes Documentation:**  
  [Kubernetes API & Concepts](https://kubernetes.io/docs/concepts/)
- **Additional Libraries:**  
  [RocksDB Rust Bindings](https://docs.rs/rocksdb/latest/rocksdb/)

---
