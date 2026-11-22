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
