-- Query: Solve rate by domain and generation
SELECT 
    e.domain,
    c.generation_id,
    COUNT(*) AS total_cells,
    COUNT(CASE WHEN c.state = 'succeeded' THEN 1 END) AS solved_cells,
    CAST(COUNT(CASE WHEN c.state = 'succeeded' THEN 1 END) AS FLOAT) / COUNT(*) AS solve_rate
FROM cells c
JOIN experiments e ON c.experiment_id = e.id
GROUP BY e.domain, c.generation_id
ORDER BY e.domain, c.generation_id;
