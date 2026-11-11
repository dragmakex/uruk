-- Campaign rounds are configured work, not a consumable resource. Legacy
-- schedulers stored a campaign that completed every round as budget-exhausted.
UPDATE runs
SET state = 'completed',
    stop_condition = 'rounds_completed'
WHERE state = 'budget-exhausted'
  AND stop_condition = 'budget_exhausted'
  AND EXISTS (
      SELECT 1
      FROM goals
      WHERE goals.run_id = runs.id
        AND goals.revision = (
            SELECT MAX(latest.revision)
            FROM goals AS latest
            WHERE latest.run_id = runs.id
        )
        AND json_extract(goals.body, '$.mode') = 'campaign'
        AND runs.iterations >= CAST(
            json_extract(goals.body, '$.budget.max_iterations') AS INTEGER
        )
  );
