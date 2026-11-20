CASE
    WHEN substr(created_at, 1, 19) GLOB
         '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9][Tt ][0-9][0-9]:[0-9][0-9]:[0-9][0-9]'
         AND {seconds} IS NOT NULL
         AND (
             length(substr(created_at, 20)) = length({suffix})
             OR (substr(created_at, 20, 2) GLOB '.[0-9]' AND {fraction} NOT GLOB '*[^0-9]*')
         )
         AND (
             {zone} IN ('Z', 'z')
             OR ({zone} GLOB '[+-][0-2][0-9]' OR {zone} GLOB '[+-][0-2][0-9][0-5][0-9]')
                 AND substr({zone}, 2, 2) <= '23'
         )
    THEN printf('%012d', 400000000000 + {seconds} -
        CASE substr({zone}, 1, 1)
            WHEN '+' THEN 1 WHEN '-' THEN -1 ELSE 0
        END * (3600 * CAST(substr({zone}, 2, 2) AS INTEGER) +
                 60 * CAST(substr({zone}, 4, 2) AS INTEGER)))
         || substr({fraction} || '000000000', 1, 9)
END
