pub(super) const MEMORY: &str = r#"
BEGIN;
PRAGMA defer_foreign_keys = ON;
INSERT INTO rh_spaces VALUES ('team', 'team', 'Team');
INSERT INTO rh_space_members VALUES ('team', 'alice', 'member');
INSERT INTO rh_user_settings (user_id, default_since, updated_at) VALUES ('alice', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
INSERT INTO rh_tasks (id, goal, status, agent, node, owner_user_id, created_at)
VALUES ('00000000-0000-0000-0000-000000000001', 'Finish the move', 'completed', 'codex', 'node-a', 'alice', '2026-01-01T00:00:00Z');
INSERT INTO ros_conversations (id, session_key, agent, title, created_at, updated_at, task_id, owner_user_id, title_from_capture)
VALUES ('00000000-0000-0000-0000-000000000002', 'codex:session', 'codex', 'Transfer', '2026-01-01T00:00:00Z',
        '2026-01-01T00:00:02Z', '00000000-0000-0000-0000-000000000001', 'alice', 1);
INSERT INTO rh_conversation_nodes VALUES ('00000000-0000-0000-0000-000000000002', 'node-a');
INSERT INTO rh_summary_capture_clock VALUES ('00000000-0000-0000-0000-000000000002', '2026-01-01T00:00:01.123456Z');
INSERT INTO rh_session_marks VALUES ('codex:session', 'alice', 'team', '2026-01-01T00:00:00Z');
INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, metadata, created_at, owner_user_id)
VALUES ('00000000-0000-0000-0000-000000000004', '00000000-0000-0000-0000-000000000002', 'codex', 'cli',
        'assistant', 'Second', '{"longer":1,"a":"do not change"}', '2026-01-01T00:00:01.123456Z', 'alice'),
       ('00000000-0000-0000-0000-000000000003', '00000000-0000-0000-0000-000000000002', 'codex', 'cli',
        'user', 'First', '{}', '2026-01-01T00:00:01.123455Z', 'alice');
UPDATE ros_messages SET tool_name = 'shell', tool_args = '{"command":"pwd"}',
    tool_result = '/tmp/source' WHERE role = 'assistant';
INSERT INTO ros_summaries (id, conversation_id, parent_id, depth, content, kind, created_at)
VALUES ('00000000-0000-0000-0000-000000000006', '00000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000099', 1, 'Branch', 'branch', '2026-01-01T00:00:00Z'),
       ('00000000-0000-0000-0000-000000000007', '00000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000006', 0, 'Leaf', 'leaf', '2026-01-01T00:00:00Z'),
       ('00000000-0000-0000-0000-000000000099', '00000000-0000-0000-0000-000000000002', NULL, 2, 'Root', 'root', '2026-01-01T00:00:00Z');
INSERT INTO ros_summaries (id, conversation_id, parent_id, depth, content, kind, created_at, pipeline_version,
    summary_ordinal, issued_part_ordinal, input_token_estimate, source_first, source_last, input_key,
    message_count, earliest_at, latest_at, range_verified, section_char_start, section_char_end, index_needs_repair)
SELECT id, '00000000-0000-0000-0000-000000000002', parent_id, depth, content, kind, '2026-01-01T00:00:00Z', 7,
       ordinal, issued, estimate, 1, 2, zeroblob(32), 2, '2026-01-01T00:00:01.123455Z', '2026-01-01T00:00:01.123456Z',
       verified, start, end, repair
FROM (
 SELECT '00000000-0000-0000-0000-000000000008' id, NULL parent_id, 2 depth, 'Session' content, 'session' kind, 0 ordinal, NULL issued, NULL estimate, NULL verified, NULL start, NULL end, 1 repair
 UNION ALL SELECT '00000000-0000-0000-0000-000000000009', '00000000-0000-0000-0000-000000000008', 1, 'Part', 'part', 1, 1, 3, NULL, NULL, NULL, 1
 UNION ALL SELECT '00000000-0000-0000-0000-000000000010', '00000000-0000-0000-0000-000000000009', 0, 'Section', 'section', 1, NULL, NULL, 1, 0, 4, 0
);
UPDATE ros_summaries SET index_key = zeroblob(32) WHERE kind = 'session';
INSERT INTO ros_summary_passages
    (id, record_id, ordinal, char_start, char_end, content, earliest_at, latest_at, index_key)
VALUES ('00000000-0000-0000-0000-000000000061', '00000000-0000-0000-0000-000000000008',
        0, 0, 3, 'Ses', '2026-01-01T00:00:01.123455Z', '2026-01-01T00:00:01.123455Z', zeroblob(32)),
       ('00000000-0000-0000-0000-000000000062', '00000000-0000-0000-0000-000000000008',
        1, 3, 7, 'sion', '2026-01-01T00:00:01.123456Z', '2026-01-01T00:00:01.123456Z', zeroblob(32));
INSERT INTO ros_summary_sources VALUES ('00000000-0000-0000-0000-000000000007', '00000000-0000-0000-0000-000000000003', 0, NULL);
INSERT INTO ros_summary_sources VALUES
       ('00000000-0000-0000-0000-000000000008', '00000000-0000-0000-0000-000000000003', 1, zeroblob(32)),
       ('00000000-0000-0000-0000-000000000009', '00000000-0000-0000-0000-000000000003', 1, zeroblob(32));
INSERT INTO ros_summary_sources VALUES
       ('00000000-0000-0000-0000-000000000008', '00000000-0000-0000-0000-000000000004', 2, zeroblob(32)),
       ('00000000-0000-0000-0000-000000000009', '00000000-0000-0000-0000-000000000004', 2, zeroblob(32));
INSERT INTO rh_summary_state (conversation_id, input_revision, last_input_at, settled_at, summarized_through, historical_only, opening_checked_user_count)
VALUES ('00000000-0000-0000-0000-000000000002', 2, '2026-01-01T00:00:01.123456Z', '2026-01-01T00:00:02Z', 2, 0, 1);
INSERT INTO rh_summary_openings VALUES ('00000000-0000-0000-0000-000000000002', 'An opening', 1, 0, 1);
INSERT INTO rh_summary_runs VALUES ('00000000-0000-0000-0000-000000000011', '00000000-0000-0000-0000-000000000002', 2,
                                    '00000000-0000-0000-0000-000000000008', '{"step":"resume"}', '2026-01-01T00:00:02Z');
INSERT INTO rh_summary_checkpoints VALUES ('00000000-0000-0000-0000-000000000011', zeroblob(32), '{"result":"Ready"}');
INSERT INTO ros_summary_deliveries (conversation_id, consumer, message_id, anchor_id)
VALUES ('00000000-0000-0000-0000-000000000002', 'wiki', '00000000-0000-0000-0000-000000000003', '00000000-0000-0000-0000-000000000008');
INSERT INTO rh_summary_conversion_items (conversation_id, last_input_at, estimated_tokens, state)
VALUES ('00000000-0000-0000-0000-000000000002', '2026-01-01T00:00:02Z', 20, 'complete');
INSERT INTO rh_topics (id, slug, name, kind, parent_id, owner_user_id, created_at, updated_at)
VALUES ('00000000-0000-0000-0000-000000000012', 'transfer', 'Transfer', 'work', NULL, 'alice', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'),
       ('00000000-0000-0000-0000-000000000013', 'quill', 'Quill', 'entity', (SELECT id FROM rh_topics WHERE slug = 'software'),
        'alice', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
INSERT INTO rh_topics (id, slug, name, kind, parent_id, owner_user_id, created_at, updated_at)
VALUES ('00000000-0000-0000-0000-000000000011', 'child', 'Child', 'work', '00000000-0000-0000-0000-000000000098', 'alice', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'),
       ('00000000-0000-0000-0000-000000000098', 'parent', 'Parent', 'work', NULL, 'alice', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
INSERT INTO rh_topic_aliases VALUES ('00000000-0000-0000-0000-000000000012', 'The Transfer', 'the-transfer', 'work');
INSERT INTO rh_topic_redirects (slug, topic_id, created_at, merged_from) VALUES ('move', '00000000-0000-0000-0000-000000000012', '2026-01-01T00:00:00Z', 'Move');
INSERT INTO rh_topic_tombstones (kind, slug, deleted_at, owner_user_id) VALUES ('work', 'discarded', '2026-01-01T00:00:00Z', 'alice');
INSERT INTO rh_topic_decisions (id, topic_id, text, status, decided_by, created_at, updated_at)
VALUES ('00000000-0000-0000-0000-000000000014', '00000000-0000-0000-0000-000000000012', 'Use the new backend', 'active', 'alice', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
INSERT INTO rh_session_placements VALUES ('codex:session', '00000000-0000-0000-0000-000000000012', 'you', '2026-01-01T00:00:00Z', 'alice');
INSERT INTO rh_conversation_placements VALUES ('00000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000012', 'you', '2026-01-01T00:00:00Z', 'alice');
INSERT INTO rh_session_suggestions (session_id, name, topic_id) VALUES ('codex:session', 'Transfer', '00000000-0000-0000-0000-000000000012');
INSERT INTO rh_session_history VALUES ('codex:session', '2026-01-01T00:00:00Z');
INSERT INTO rh_session_startup (session_id, topic_id, tokens, sections, created_at, delivered_via)
VALUES ('codex:session', '00000000-0000-0000-0000-000000000012', 5, '[]', '2026-01-01T00:00:00Z', 'metaAppend');
INSERT INTO rh_filing_corrections (id, conversation_id, from_topic, to_topic, corrected_at)
VALUES ('00000000-0000-0000-0000-000000000015', '00000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000012',
        '00000000-0000-0000-0000-000000000012', '2026-01-01T00:00:00Z');
INSERT INTO ros_wiki_topics (slug, title, aliases, current_state, article, search_text, created_at, updated_at)
VALUES ('transfer', 'Transfer', '["Move"]', 'Moving stores', 'The article', 'The article', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
INSERT INTO rh_topic_pages (page_slug, topic_id, sections)
VALUES ('transfer', '00000000-0000-0000-0000-000000000012', '[{"heading":"Archive","body":"Archived section text"}]');
INSERT INTO rh_wiki_scopes VALUES ('alice-team', 'alice', 'team', 'team-', 'spaces/team/alice');
UPDATE ros_wiki_topics SET visibility = 'team', scope = 'alice-team', local_slug = 'the-transfer' WHERE slug = 'transfer';
INSERT INTO rh_agent_presets (id, name, harness_id, node, directory, shared_link, system_prompt, sort_order, created_at, updated_at, owner_user_id)
VALUES ('one', 'Writer', 'codex', 'node-a', '/tmp/project', 0, 'Read the archive first.', 8, '2026-01-01T00:00:01.123455Z', '2026-01-01T00:00:01.123456Z', 'alice');
INSERT INTO ros_wiki_extractions (summary_id, status, pipeline_version, topics_touched, extracted_at)
VALUES ('00000000-0000-0000-0000-000000000008', 'done', 7, '["transfer"]', '2026-01-01T00:00:00Z');
INSERT INTO ros_wiki_citations VALUES ('transfer', '00000000-0000-0000-0000-000000000008', NULL, 'Source', '2026-01-01T00:00:00Z');
INSERT INTO ros_wiki_provenance VALUES ('transfer', 'summary', '00000000-0000-0000-0000-000000000008', '00000000-0000-0000-0000-000000000002', 'abc', '2026-01-01T00:00:00Z');
INSERT INTO rh_wiki_entity_candidates VALUES ('candidate', 'source', '00000000-0000-0000-0000-000000000008', '{}', '2026-01-01T00:00:00Z', 'alice');
INSERT INTO rh_wiki_deferred VALUES ('00000000-0000-0000-0000-000000000008', 'codex:session', NULL, NULL, '[]', '2026-01-01T00:00:00Z', 'alice');
INSERT INTO ros_wiki_redirects VALUES ('move', 'transfer', '2026-01-01T00:00:00Z', 'alice');
INSERT INTO ros_tags (id, entity_type, entity_id, key, value, source, state, created_at, updated_at)
VALUES ('00000000-0000-0000-0000-000000000016', 'conversation', '00000000-0000-0000-0000-000000000002', 'topic', 'transfer', 'user', 'accepted', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
INSERT INTO ros_tag_taxonomy (key, value, display, aliases, state, source, created_at, updated_at, owner_user_id)
VALUES ('topic', 'transfer', 'Transfer', '["move"]', 'active', 'manual', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'alice');
INSERT INTO rh_entity_extractions (summary_id, status, pipeline_version, extracted_at, owner_user_id)
VALUES ('00000000-0000-0000-0000-000000000008', 'done', 7, '2026-01-01T00:00:00Z', 'alice');
INSERT INTO rh_entity_history VALUES ('00000000-0000-0000-0000-000000000013', '00000000-0000-0000-0000-000000000008',
                                      '00000000-0000-0000-0000-000000000002', '2026-01-01', 'Used Quill.', 'alice');
INSERT INTO rh_entity_candidates VALUES ('00000000-0000-0000-0000-000000000017', '00000000-0000-0000-0000-000000000008',
                                         'Quill', 'software', 0.91, 'In the transcript', '{}', '2026-01-01T00:00:00Z', 'alice');
INSERT INTO rh_entity_decisions VALUES ('00000000-0000-0000-0000-000000000018', '00000000-0000-0000-0000-000000000008',
                                        'entity', 'Quill', 'software', '[]', 'none', NULL, 0.91, 'test-model', '00000000-0000-0000-0000-000000000013', '2026-01-01T00:00:00Z', 'alice');
INSERT INTO rh_entity_embeddings VALUES ('00000000-0000-0000-0000-000000000013', CAST(X'CDCCCC3D' || zeroblob(4092) AS BLOB), '2026-01-01T00:00:00Z', 'alice');
INSERT INTO rh_entity_applied VALUES ('00000000-0000-0000-0000-000000000013', '2026-01-01T00:00:00Z', '00000000-0000-0000-0000-000000000008', 'alice');
INSERT INTO rh_entity_type_seen VALUES ('00000000-0000-0000-0000-000000000013', '00000000-0000-0000-0000-000000000008', 'software', '2026-01-01T00:00:00Z', 'alice');
INSERT INTO rh_entity_facts VALUES ('00000000-0000-0000-0000-000000000019', '00000000-0000-0000-0000-000000000013', 'type', '2026-01-01T00:00:00Z');
INSERT INTO rh_entity_fact_claims (id, fact_id, value, actor, pinned, at, recorded_at, owner_user_id)
VALUES ('00000000-0000-0000-0000-000000000020', '00000000-0000-0000-0000-000000000019', 'Editor', 'you', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'alice');
INSERT INTO rh_tidy_batches VALUES ('00000000-0000-0000-0000-000000000021', 'categorizer', '2026-01-01T00:00:00Z');
INSERT INTO rh_tidy_proposals (id, batch_id, position, fingerprint, change, reason, evidence)
VALUES ('00000000-0000-0000-0000-000000000022', '00000000-0000-0000-0000-000000000021', 0, 'transfer', '{}', 'tidy', '[]');
INSERT INTO rh_review_marks VALUES ('alice', 'transfer', 'decided', '2026-01-01T00:00:00Z');
INSERT INTO rh_session_trash VALUES ('codex:trashed', '2026-01-01T00:00:00Z', 'alice');
INSERT INTO rh_session_tombstones VALUES ('codex:deleted', '2026-01-01T00:00:00Z', 'alice');
INSERT INTO rh_session_keeps VALUES ('codex:session', '2026-01-01T00:00:00Z', 'alice');
INSERT INTO rh_daily_digests VALUES ('alice', '2026-01-01', 'Transfer complete', 4, '2026-01-01T00:00:00Z');
INSERT INTO rh_model_calls (call_id, owner_user_id, session_key, started_at, provider, outcome)
VALUES ('test-call', 'alice', 'codex:session', '2026-01-01T00:00:00Z', 'test', 'completed');
COMMIT;
"#;

pub(super) const FOREIGN_MEMORY: &str = r"
INSERT INTO rh_spaces VALUES ('other-team', 'team', 'Other team');
INSERT INTO rh_space_members VALUES ('other-team', 'bob', 'member');
INSERT INTO rh_space_members (space_id, user_id, role)
SELECT id, 'bob', 'member' FROM rh_spaces WHERE id = 'team';
INSERT INTO rh_topics (id, slug, name, kind, owner_user_id, created_at, updated_at)
VALUES (gen_random_uuid(), 'cedar-owned-delete', 'Cedar owned delete', 'entity', 'bob', now(), now());
INSERT INTO rh_topic_tombstones (kind, slug, deleted_at) VALUES ('entity', 'cedar-owned-delete', now());
DELETE FROM rh_topics WHERE slug = 'cedar-owned-delete';
INSERT INTO rh_topic_tombstones (kind, slug, deleted_at) VALUES ('entity', 'secret-deal', now());
INSERT INTO rh_summaries (id, content, kind, owner_user_id, created_at)
VALUES ('10000000-0000-0000-0000-000000000063', 'Private', 'leaf', 'bob', now());
INSERT INTO rh_summary_passages
    (id, record_id, ordinal, char_start, char_end, content, earliest_at, latest_at, index_key)
VALUES ('10000000-0000-0000-0000-000000000064', '10000000-0000-0000-0000-000000000063',
    0, 0, 7, 'Private', now(), now(), decode(repeat('ab', 32), 'hex'));
INSERT INTO rh_tags (entity_type, entity_id, key, value, source, state, owner_user_id)
SELECT 'conversation', id, 'topic', 'secret-deal', 'user', 'accepted', owner_user_id FROM rh_conversations WHERE session_key = 'other-session';
INSERT INTO rh_tag_taxonomy (key, value, display, source) VALUES ('topic', 'global-catalog', 'Global catalog', 'user');
INSERT INTO rh_topics (id, slug, name, kind, created_at, updated_at) VALUES (gen_random_uuid(), 'global-catalog', 'Global catalog', 'type', now(), now());
INSERT INTO rh_topic_aliases (topic_id, alias, alias_key, kind)
SELECT id, 'Global alias', 'global-alias', 'type' FROM rh_topics WHERE slug = 'global-catalog';
INSERT INTO rh_topic_redirects (slug, topic_id, created_at)
SELECT 'global-redirect', id, now() FROM rh_topics WHERE slug = 'global-catalog';
INSERT INTO rh_topic_decisions (id, topic_id, text, status, decided_by, created_at, updated_at)
SELECT gen_random_uuid(), id, 'Global decision', 'active', 'bob', now(), now() FROM rh_topics WHERE slug = 'global-catalog';
INSERT INTO rh_entity_facts (id, topic_id, key, created_at)
SELECT '10000000-0000-0000-0000-000000000019', id, 'secret-fact', now() FROM rh_topics WHERE slug = 'software';
INSERT INTO rh_entity_fact_claims (id, fact_id, value, actor, owner_user_id, at, recorded_at)
VALUES (gen_random_uuid(), '10000000-0000-0000-0000-000000000019', 'Cedar claim', 'you', 'bob', now(), now());
";

pub(super) const HUB_MEMORY: &str = r#"
INSERT INTO rh_topics (id, slug, name, kind, owner_user_id, created_at, updated_at)
VALUES (gen_random_uuid(), 'owned-delete', 'Owned deleted topic', 'work', 'alice', now(), now());
INSERT INTO rh_topic_tombstones (kind, slug, deleted_at) VALUES ('work', 'owned-delete', now());
DELETE FROM rh_topics WHERE slug = 'owned-delete';
UPDATE rh_wiki_topics SET archived_sections = '[{"heading":"Hub archive","body":"Compiled on the hub"}]'
WHERE slug = 'transfer';
INSERT INTO rh_wiki_topics (slug, title, topic_id, owner_user_id)
VALUES ('bound-no-archive', 'Bound page', '00000000-0000-0000-0000-000000000011', 'alice');
INSERT INTO rh_wiki_topics (slug, title, archived_sections, owner_user_id)
VALUES ('archive-no-binding', 'Unbound archive', '[{"heading":"Archived","body":"No topic"}]', 'alice');
INSERT INTO rh_entity_facts (id, topic_id, key, created_at)
SELECT '30000000-0000-0000-0000-000000000019', id, 'alice-fact', now() FROM rh_topics WHERE slug = 'software';
INSERT INTO rh_entity_fact_claims (id, fact_id, value, actor, owner_user_id, at, recorded_at)
VALUES ('30000000-0000-0000-0000-000000000020', '30000000-0000-0000-0000-000000000019',
        'Alice claim', 'you', 'alice', now(), now());
INSERT INTO rh_message_chunks (message_id, idx, char_start, char_end, content, owner_user_id, conversation_id, said)
VALUES ('00000000-0000-0000-0000-000000000003', 0, 0, 5, 'First', 'alice',
        '00000000-0000-0000-0000-000000000002', true);
UPDATE rh_summaries SET index_key = decode(repeat('ab', 32), 'hex')
WHERE id = '00000000-0000-0000-0000-000000000009';
INSERT INTO rh_summary_passages (id, record_id, ordinal, char_start, char_end, content, earliest_at, latest_at, index_key)
VALUES (gen_random_uuid(), '00000000-0000-0000-0000-000000000009', 0, 0, 4, 'Part',
        '2026-01-01T00:00:01.123455Z', '2026-01-01T00:00:01.123456Z', decode(repeat('ab', 32), 'hex'));
"#;

pub(super) const UNSORTED_CHILDREN: &str = r"
INSERT INTO rh_topics (id, slug, name, kind, parent_id, owner_user_id, created_at, updated_at)
VALUES ('00000000-0000-0000-0000-000000000001', 'child', 'Child', 'work',
        '00000000-0000-0000-0000-000000000009', 'alice', now(), now());
INSERT INTO rh_summaries (id, parent_id, depth, content, kind, created_at, owner_user_id)
VALUES ('00000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000008',
        0, 'Child', 'leaf', now(), 'alice');
";

pub(super) const UNSORTED_PARENTS: &str = r"
INSERT INTO rh_topics (id, slug, name, kind, owner_user_id, created_at, updated_at)
VALUES ('00000000-0000-0000-0000-000000000009', 'parent', 'Parent', 'work', 'alice', now(), now());
INSERT INTO rh_summaries (id, depth, content, kind, created_at, owner_user_id)
VALUES ('00000000-0000-0000-0000-000000000008', 1, 'Parent', 'branch', now(), 'alice');
";
