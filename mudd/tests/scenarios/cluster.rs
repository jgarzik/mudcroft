//! Multi-node: joining, replication, restarts, refusals

use std::time::Duration;

use crate::harness::TestCluster;

async fn register(cluster: &TestCluster, node: usize, username: &str) -> reqwest::StatusCode {
    cluster
        .client
        .post(format!("{}/auth/register", cluster.nodes[node].base_url()))
        .json(&serde_json::json!({"username": username, "password": "testpass123"}))
        .send()
        .await
        .unwrap_or_else(|e| panic!("register on node {}: {}\n{}", node + 1, e, cluster.logs()))
        .status()
}

fn account_count(username: &str) -> String {
    format!(
        "SELECT COUNT(*) FROM accounts WHERE username = '{}'",
        username
    )
}

/// Two empty nodes join node 1: each starts from a snapshot (node 1's
/// initial data predates the log) and then follows the log
#[tokio::test]
async fn test_cluster_join_and_replicate() {
    let cluster = TestCluster::start(3).await.expect("cluster");
    // Node 1's mudd_init data arrived by snapshot
    for node in 1..3 {
        assert_eq!(
            cluster
                .wait_for(
                    node,
                    &account_count("cluster_admin"),
                    1,
                    Duration::from_secs(20)
                )
                .await,
            1,
            "node {} has node 1's initial data",
            node + 1
        );
    }
    // A write on the leader reaches every member
    assert!(register(&cluster, 0, "replicated_hero").await.is_success());
    for node in 1..3 {
        assert_eq!(
            cluster
                .wait_for(
                    node,
                    &account_count("replicated_hero"),
                    1,
                    Duration::from_secs(20)
                )
                .await,
            1,
            "node {} has the new account",
            node + 1
        );
    }
}

/// A member that was down catches up when it restarts (no --join needed:
/// it knows its id)
#[tokio::test]
async fn test_cluster_member_restart_catches_up() {
    let mut cluster = TestCluster::start(3).await.expect("cluster");
    cluster
        .wait_for(
            2,
            &account_count("cluster_admin"),
            1,
            Duration::from_secs(20),
        )
        .await;
    cluster.stop(2);
    assert!(register(&cluster, 0, "while_away").await.is_success());
    cluster.restart(2).await.expect("restart");
    assert_eq!(
        cluster
            .wait_for(2, &account_count("while_away"), 1, Duration::from_secs(20))
            .await,
        1
    );
}

/// A node with the wrong secret is refused and exits
#[tokio::test]
async fn test_cluster_wrong_secret_refused() {
    let mut cluster = TestCluster::start(1).await.expect("cluster");
    let wrong = cluster.dir.path().join("wrong-secret");
    std::fs::write(&wrong, "not-the-cluster-secret-at-all").unwrap();
    let err = cluster.add_joiner(Some(&wrong)).await.expect_err("refused");
    assert!(err.to_string().contains("authentication"), "{}", err);
}

/// Members of `mudd_admin cluster status` output: (id, voter)
fn members(status: &str) -> Vec<(u64, bool)> {
    let v: serde_json::Value = serde_json::from_str(status).expect(status);
    v["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["node_id"].as_u64().unwrap(),
                m["voter"].as_bool().unwrap(),
            )
        })
        .collect()
}

/// mudd_admin inspects the cluster through any member and removes a node
/// (through the leader); the last voter cannot be removed; a removed node
/// refuses to start again
#[tokio::test]
async fn test_cluster_admin_status_and_remove() {
    let mut cluster = TestCluster::start(3).await.expect("cluster");
    // All three become voters
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut status = String::new();
    while tokio::time::Instant::now() < deadline {
        let (ok, out) = cluster.admin(1, &["status"]).unwrap();
        assert!(ok, "{}", out);
        status = out;
        if members(&status) == vec![(1, true), (2, true), (3, true)] {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(
        members(&status),
        vec![(1, true), (2, true), (3, true)],
        "{}",
        status
    );

    // Asked through a follower, the removal reaches the leader
    let (ok, out) = cluster.admin(1, &["remove", "3"]).unwrap();
    assert!(ok, "{}\n{}", out, cluster.logs());
    let (_, status) = cluster.admin(0, &["status"]).unwrap();
    assert_eq!(members(&status), vec![(1, true), (2, true)], "{}", status);
    let (ok, out) = cluster.admin(0, &["remove", "99"]).unwrap();
    assert!(!ok && out.contains("not a member"), "{}", out);

    // The removed node cannot come back with its old id
    cluster.stop(2);
    let restarted = cluster.restart(2).await;
    let err = match restarted {
        Ok(()) => panic!("removed node restarted\n{}", cluster.logs()),
        Err(e) => e,
    };
    assert!(err.to_string().contains("was removed"), "{}", err);

    // Down to one voter, which cannot be removed
    let (ok, out) = cluster.admin(0, &["remove", "2"]).unwrap();
    assert!(ok, "{}", out);
    let (ok, out) = cluster.admin(0, &["remove", "1"]).unwrap();
    assert!(!ok && out.contains("last voter"), "{}", out);
}
