#!/usr/bin/env python3
import subprocess
import time
import argparse
import statistics
import json
import re
import matplotlib.pyplot as plt
from kubernetes import client, config

# Parse arguments
parser = argparse.ArgumentParser(description='Benchmark OmniPaxos recovery')
parser.add_argument('--runs', type=int, default=3, help='Number of test runs')
parser.add_argument('--load', type=int, default=1000, help='Number of key-value pairs to load')
parser.add_argument('--key-size', type=int, default=10, help='Size of keys in bytes')
parser.add_argument('--value-size', type=int, default=100, help='Size of values in bytes')
parser.add_argument('--output', type=str, default='benchmark_results.json', help='Output file for results')
parser.add_argument('--pod-to-kill', type=str, default='kv-store-2', help='Pod to kill for recovery test')
parser.add_argument('--single-key', action='store_true', help='Use a single key with multiple updates')
parser.add_argument('--memory-store', action='store_true', help='Use in-memory storage (no persistence)')
args = parser.parse_args()

# Initialize Kubernetes client
config.load_kube_config()
v1 = client.CoreV1Api()

def generate_workload(num_entries, key_size, value_size, single_key=False):
    """Generate a workload of PUT operations
    
    If single_key is True, will update the same key multiple times
    """
    workload = []
    if single_key:
        # For single key case, use the same key but different values
        key = f"key{'0'*(key_size-3)}"[:key_size]  # Single consistent key
        for i in range(num_entries):
            value = f"value{i:0{value_size-5}d}"[:value_size]  # Different values
            workload.append(f"put {key} {value}")
    else:
        # Original implementation for multiple keys
        for i in range(num_entries):
            key = f"key{i:0{key_size-3}d}"[:key_size]  # Ensure key size
            value = f"value{i:0{value_size-5}d}"[:value_size]  # Ensure value size
            workload.append(f"put {key} {value}")
    return workload

def execute_command(cmd):
    """Execute a command on the net pod"""
    result = subprocess.run(
        ["kubectl", "exec", "-i", "net", "--", "/bin/sh", "-c", cmd],
        capture_output=True, text=True
    )
    return result.stdout

def load_data(workload):
    """Load data into the cluster"""
    start_time = time.time()
    
    # Prepare command batch (send multiple commands at once)
    commands = "\n".join(workload)
    
    # Execute through kubectl
    print(f"Loading {len(workload)} items...")
    execute_command(f"echo '{commands}' | /app/net_actor")
    
    end_time = time.time()
    print(f"Data loading completed in {end_time - start_time:.2f} seconds")

def measure_network_traffic_start():
    """Start measuring network traffic on all pods"""
    for pod_name in ["kv-store-0", "kv-store-1", "kv-store-2"]:
        try:
            execute_command(f"cat /proc/net/dev > /tmp/netstat_start_{pod_name}")
        except Exception as e:
            print(f"Warning: Could not capture start network stats for {pod_name}: {e}")

def measure_network_traffic_end():
    """End measuring network traffic and calculate delta"""
    results = {}
    for pod_name in ["kv-store-0", "kv-store-1", "kv-store-2"]:
        try:
            start = execute_command(f"cat /tmp/netstat_start_{pod_name}")
            current = execute_command(f"cat /proc/net/dev")
            
            # Parse network stats
            start_bytes = parse_network_stats(start)
            end_bytes = parse_network_stats(current)
            
            # Calculate delta
            delta_rx = end_bytes['rx'] - start_bytes['rx']
            delta_tx = end_bytes['tx'] - start_bytes['tx']
            
            results[pod_name] = {
                'rx_bytes': delta_rx,
                'tx_bytes': delta_tx,
                'total_bytes': delta_rx + delta_tx
            }
        except Exception as e:
            print(f"Warning: Could not calculate network stats for {pod_name}: {e}")
            results[pod_name] = {'rx_bytes': 0, 'tx_bytes': 0, 'total_bytes': 0}
    
    return results

def parse_network_stats(netstat_output):
    """Parse /proc/net/dev output to extract bytes received/transmitted"""
    # Find eth0 line
    for line in netstat_output.splitlines():
        if 'eth0:' in line:
            parts = line.split()
            return {'rx': int(parts[1]), 'tx': int(parts[9])}
    return {'rx': 0, 'tx': 0}

def pod_exists(pod_name):
    """Check if pod exists and is not terminating"""
    try:
        pods = v1.list_namespaced_pod(namespace="default")
        for pod in pods.items:
            if pod.metadata.name == pod_name:
                if pod.status.phase != "Terminating":
                    return True
        return False
    except Exception as e:
        print(f"Error checking pod status: {e}")
        return False

def benchmark_recovery():
    """Run the recovery benchmark"""
    results = []
    
    for run in range(args.runs):
        print(f"\n=== Run {run+1}/{args.runs} ===")
        
        # Generate and load data
        workload = generate_workload(args.load, args.key_size, args.value_size, args.single_key)
        load_data(workload)
        
        # Verify data was loaded
        time.sleep(3)  # Wait for replication to complete
        print("Verifying data...")
        sample_key = workload[0].split()[1]
        result = execute_command(f"echo 'get {sample_key}' | /app/net_actor")
        print(f"Sample key verification: {result}")
        
        # Kill a node (not the first one)
        pod_to_kill = args.pod_to_kill
        port_to_test = "8003" if pod_to_kill == "kv-store-2" else "8002"  # Port based on pod
        
        print(f"Killing {pod_to_kill}...")
        subprocess.run(["kubectl", "delete", "pod", pod_to_kill, "--force", "--grace-period=0"],
                      capture_output=True)
        
        # Wait for pod to terminate with a timeout
        print("Waiting for pod to terminate...")
        terminate_start = time.time()
        while time.time() - terminate_start < 30:  # 30 second timeout
            if not pod_exists(pod_to_kill):
                print(f"Pod {pod_to_kill} terminated successfully")
                break
            time.sleep(1)
        else:
            print(f"Warning: Pod {pod_to_kill} did not terminate within timeout, continuing anyway")
        
        # Start network traffic measurement
        measure_network_traffic_start()
        
        # Start timing recovery
        start_time = time.time()
        
        # Wait for pod to be recreated with timeout
        print("Waiting for pod to be recreated...")
        recreate_start = time.time()
        pod_recreated = False
        while time.time() - recreate_start < 60:  # 60 second timeout
            pods = v1.list_namespaced_pod(namespace="default")
            if any(pod.metadata.name == pod_to_kill and pod.status.phase == "Running" 
                  for pod in pods.items):
                pod_recreated = True
                print(f"Pod {pod_to_kill} recreated successfully")
                break
            time.sleep(1)
        
        if not pod_recreated:
            print(f"Warning: Pod {pod_to_kill} was not recreated within timeout")
        
        # Wait for pod to be ready
        print("Waiting for pod to be ready...")
        ready = False
        recovery_time = None
        
        # Monitor the logs to detect when recovery is complete
        log_check_start = time.time()
        while time.time() - log_check_start < 120:  # 2 minute timeout
            try:
                logs = subprocess.run(
                    ["kubectl", "logs", pod_to_kill, "--tail=50"],
                    capture_output=True, text=True
                ).stdout
                
                if "Recovery complete" in logs:
                    recovery_time = time.time() - start_time
                    ready = True
                    print(f"Recovery message detected in logs")
                    break
            except Exception as e:
                print(f"Warning: Could not get logs: {e}")
            
            # Also check if the node can process requests
            try:
                test_result = execute_command(f"echo 'get {sample_key} {port_to_test}' | /app/net_actor")
                if sample_key in test_result and "value" in test_result:
                    recovery_time = time.time() - start_time
                    ready = True
                    print(f"Node can process requests successfully")
                    break
            except Exception as e:
                print(f"Warning: Error testing node availability: {e}")
                
            time.sleep(2)
        
        # Measure network traffic
        network_stats = measure_network_traffic_end()
        
        if not ready:
            print("WARNING: Pod did not recover properly within timeout")
            recovery_time = 120  # Use timeout value
        
        # Record results
        run_result = {
            "run": run + 1,
            "recovery_time_seconds": recovery_time,
            "network_traffic": network_stats,
            "data_size": args.load * (args.key_size + args.value_size),
        }
        
        results.append(run_result)
        print(f"Recovery took {recovery_time:.2f} seconds")
        print(f"Network traffic: {json.dumps(network_stats, indent=2)}")
        
        # Wait before next run
        time.sleep(10)
    
    return results

def generate_report(results):
    """Generate and save benchmark report"""
    # Calculate averages
    recovery_times = [r["recovery_time_seconds"] for r in results]
    avg_recovery_time = statistics.mean(recovery_times)
    
    # Calculate network traffic
    total_bytes = [sum(pod_stats["total_bytes"] for pod_name, pod_stats in r["network_traffic"].items())
                 for r in results]
    avg_total_bytes = statistics.mean(total_bytes)
    
    report = {
        "test_config": {
            "runs": args.runs,
            "load_size": args.load,
            "key_size": args.key_size,
            "value_size": args.value_size,
            "pod_killed": args.pod_to_kill,
            "single_key": args.single_key,
            "memory_store": args.memory_store
        },
        "results": results,
        "summary": {
            "avg_recovery_time_seconds": avg_recovery_time,
            "min_recovery_time_seconds": min(recovery_times),
            "max_recovery_time_seconds": max(recovery_times),
            "avg_network_traffic_bytes": avg_total_bytes,
        }
    }
    
    # File naming to include test type
    test_type = "single_key" if args.single_key else "multi_key"
    storage_type = "memory" if args.memory_store else "persistent"
    output_file = f"benchmark_{test_type}_{storage_type}.json"
    
    # Save report
    with open(output_file, 'w') as f:
        json.dump(report, f, indent=2)
    
    print(f"\nBenchmark report saved to {output_file}")
    
    # Graph title adjustment to show test type
    title_suffix = " (Single Key)" if args.single_key else f" ({args.load} Keys)"
    title_suffix += " - Memory Storage" if args.memory_store else " - Persistent Storage"
    
    # Create visualization
    plt.figure(figsize=(12, 6))
    
    # Recovery time plot
    plt.subplot(1, 2, 1)
    plt.bar(range(1, args.runs + 1), recovery_times)
    plt.axhline(y=avg_recovery_time, color='r', linestyle='-', label=f'Avg: {avg_recovery_time:.2f}s')
    plt.xlabel('Run')
    plt.ylabel('Recovery Time (seconds)')
    plt.title(f'Recovery Time per Run{title_suffix}')
    plt.legend()
    
    # Network traffic plot
    plt.subplot(1, 2, 2)
    plt.bar(range(1, args.runs + 1), [b/1024/1024 for b in total_bytes])
    plt.axhline(y=avg_total_bytes/1024/1024, color='r', linestyle='-', 
                label=f'Avg: {avg_total_bytes/1024/1024:.2f} MB')
    plt.xlabel('Run')
    plt.ylabel('Network Traffic (MB)')
    plt.title(f'Network Traffic per Run{title_suffix}')
    plt.legend()
    
    plt.tight_layout()
    plt.savefig(f"{output_file.split('.')[0]}.png")
    print(f"Visualization saved to {output_file.split('.')[0]}.png")

if __name__ == "__main__":
    results = benchmark_recovery()
    generate_report(results)
