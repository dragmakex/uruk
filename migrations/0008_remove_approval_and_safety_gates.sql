-- Approval and safety gates were removed from the run contract.
--
-- Work parked for an approval already completed preparation, so it can resume.
-- A safety-blocked start never completed source ingestion or planning; settle it
-- as an incomplete start instead of executing a partial run without its inputs.
UPDATE tasks
SET state = 'pending', body = json_set(body, '$.state', 'pending')
WHERE state = 'waiting_for_human';

UPDATE runs
SET state = 'running'
WHERE state = 'waiting-for-human';

UPDATE run_queue
SET state = 'done',
    finished_at = COALESCE(
        finished_at,
        (SELECT updated_at FROM runs WHERE runs.id = run_queue.run_id)
    )
WHERE state <> 'done'
  AND run_id IN (SELECT id FROM runs WHERE state = 'blocked');

UPDATE runs
SET state = 'failed',
    stop_condition = 'start_incomplete'
WHERE state = 'blocked';

-- Plans created before this migration may carry the removed stop condition.
UPDATE plans
SET body = json_set(
    body,
    '$.stopping_conditions',
    json(COALESCE(
        (
            SELECT json_group_array(value)
            FROM json_each(plans.body, '$.stopping_conditions')
            WHERE value <> 'safety_boundary'
        ),
        '[]'
    ))
)
WHERE EXISTS (
    SELECT 1
    FROM json_each(plans.body, '$.stopping_conditions')
    WHERE value = 'safety_boundary'
);

-- Removed decision variants cannot be produced or deserialized by the current
-- contract. Other historical decisions and all usage records remain intact.
DELETE FROM decisions WHERE kind IN ('approval', 'safety_block');

DROP TABLE IF EXISTS approvals;
