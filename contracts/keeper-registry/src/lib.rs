pub fn cancel_task(e: Env, owner: Address, task_id: u64) -> Result<(), KeeperError> {
    owner.require_auth();

    let mut task = load_task(&e, task_id)?;
    if task.owner != owner {
        return Err(KeeperError::NotTaskOwner);
    }
    if task.status != TaskStatus::Pending {
        return Err(KeeperError::InvalidTaskStatus);
    }

    bump_instance(&e);

    // Effects before interaction: a re-entrant cancel must find the task
    // already Cancelled and be rejected by the status guard above.
    let refund = task.reward;
    task.status = TaskStatus::Cancelled;
    save_task(&e, task_id, &task);

    // Interaction
    reward_token(&e)?.transfer(
        &e.current_contract_address(),
        &owner,
        &refund,
    );

    emit_task_cancelled(&e, task_id, &owner);
    log!(
        &e,
        "Task {} cancelled, {} refunded to {}",
        task_id,
        refund,
        owner
    );

    Ok(())
}