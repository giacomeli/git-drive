mod common;

use common::init_repo;

#[test]
fn init_repo_produces_repo_with_resolvable_head() {
    let test_repo = init_repo();
    let head = test_repo.repo.head().expect("HEAD deve existir após init_repo");
    assert!(head.is_branch(), "HEAD deve apontar para um branch");
    assert!(
        test_repo.dir.path().join(".git").exists(),
        ".git deve existir no dir temporário"
    );
}
