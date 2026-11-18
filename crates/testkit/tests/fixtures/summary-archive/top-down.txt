-- Invented workshop transcript and mixed archive graph
COPY public.ros_conversations (id, session_key, agent, channel, created_at, updated_at, owner_user_id) FROM stdin;
00000000-0000-4000-8000-000000000001	cli:top-down	rivet	cli	2026-10-10T12:00:00.000Z	2026-10-10T12:00:00.000Z	alice
\.
COPY public.ros_messages (id, conversation_id, agent, channel, role, content, metadata, created_at, owner_user_id) FROM stdin;
00000000-0000-4000-8000-000000000101	00000000-0000-4000-8000-000000000001	rivet	cli	user	Measure the tray	{}	2026-10-10T12:01:00.000Z	alice
00000000-0000-4000-8000-000000000102	00000000-0000-4000-8000-000000000001	rivet	cli	user	Sketch the bracket	{}	2026-10-10T12:02:00.000Z	alice
00000000-0000-4000-8000-000000000103	00000000-0000-4000-8000-000000000001	rivet	cli	user	Label the fasteners	{}	2026-10-10T12:03:00.000Z	alice
00000000-0000-4000-8000-000000000104	00000000-0000-4000-8000-000000000001	rivet	cli	user	File the drawing	{}	2026-10-10T12:04:00.000Z	alice
\.
COPY public.ros_summaries (id, conversation_id, parent_id, depth, kind, content, message_count, earliest_at, latest_at, created_at, owner_user_id, pipeline_version, summary_ordinal, source_first, source_last, input_key, range_verified, section_char_start, section_char_end, issued_part_ordinal, input_token_estimate, quality_flags) FROM stdin;
00000000-0000-4000-8000-000000000010	00000000-0000-4000-8000-000000000001	\N	2	root	Old overview	4	2026-10-10T12:01:00.000Z	2026-10-10T12:04:00.000Z	2026-10-10T12:30:00.000Z	alice	6	\N	\N	\N	\N	\N	\N	\N	\N	\N	0
00000000-0000-4000-8000-000000000011	00000000-0000-4000-8000-000000000001	00000000-0000-4000-8000-000000000010	1	branch	Old activities	4	2026-10-10T12:01:00.000Z	2026-10-10T12:04:00.000Z	2026-10-10T12:30:00.000Z	alice	6	\N	\N	\N	\N	\N	\N	\N	\N	\N	0
00000000-0000-4000-8000-000000000012	00000000-0000-4000-8000-000000000001	00000000-0000-4000-8000-000000000011	0	leaf	Old notes	1	2026-10-10T12:04:00.000Z	2026-10-10T12:04:00.000Z	2026-10-10T12:30:00.000Z	alice	6	\N	\N	\N	\N	\N	\N	\N	\N	\N	0
00000000-0000-4000-8000-000000000020	00000000-0000-4000-8000-000000000001	\N	2	session	Tray, bracket and fasteners.	3	2026-10-10T12:01:00.000Z	2026-10-10T12:03:00.000Z	2026-10-10T12:30:00.000Z	alice	7	0	1	3	\\xabababababababababababababababababababababababababababababababab	\N	\N	\N	\N	\N	0
00000000-0000-4000-8000-000000000021	00000000-0000-4000-8000-000000000001	00000000-0000-4000-8000-000000000020	1	part	Tray and fasteners.	2	2026-10-10T12:01:00.000Z	2026-10-10T12:03:00.000Z	2026-10-10T12:30:00.000Z	alice	7	1	1	3	\\xabababababababababababababababababababababababababababababababab	\N	\N	\N	1	50	0
00000000-0000-4000-8000-000000000022	00000000-0000-4000-8000-000000000001	00000000-0000-4000-8000-000000000020	0	section	Fasteners	1	2026-10-10T12:03:00.000Z	2026-10-10T12:03:00.000Z	2026-10-12T12:30:00.000Z	alice	7	0	3	3	\\xabababababababababababababababababababababababababababababababab	t	18	27	\N	\N	1
00000000-0000-4000-8000-000000000023	00000000-0000-4000-8000-000000000001	00000000-0000-4000-8000-000000000021	0	section	Tray and fasteners.	2	2026-10-10T12:01:00.000Z	2026-10-10T12:03:00.000Z	2026-10-10T12:30:00.000Z	alice	7	0	1	3	\\xabababababababababababababababababababababababababababababababab	f	0	19	\N	\N	0
\.
COPY public.ros_summary_sources (summary_id, message_id, ordinal, render_key) FROM stdin;
00000000-0000-4000-8000-000000000012	00000000-0000-4000-8000-000000000104	0	\N
00000000-0000-4000-8000-000000000020	00000000-0000-4000-8000-000000000101	1	\\xabababababababababababababababababababababababababababababababab
00000000-0000-4000-8000-000000000020	00000000-0000-4000-8000-000000000102	2	\\xabababababababababababababababababababababababababababababababab
00000000-0000-4000-8000-000000000020	00000000-0000-4000-8000-000000000103	3	\\xabababababababababababababababababababababababababababababababab
00000000-0000-4000-8000-000000000021	00000000-0000-4000-8000-000000000101	1	\\xabababababababababababababababababababababababababababababababab
00000000-0000-4000-8000-000000000021	00000000-0000-4000-8000-000000000103	3	\\xabababababababababababababababababababababababababababababababab
\.
COPY public.ros_summary_deliveries (conversation_id, consumer, message_id, anchor_id, completed_version, historical_backlog) FROM stdin;
00000000-0000-4000-8000-000000000001	wiki	00000000-0000-4000-8000-000000000103	00000000-0000-4000-8000-000000000021	\N	f
00000000-0000-4000-8000-000000000001	entity	00000000-0000-4000-8000-000000000104	\N	1	f
\.
COPY public.ros_summary_passages (id, record_id, ordinal, char_start, char_end, content, earliest_at, latest_at, index_key, embedding) FROM stdin;
00000000-0000-4000-8000-000000000031	00000000-0000-4000-8000-000000000021	0	0	19	Tray and fasteners.	2026-10-10T12:01:00.000Z	2026-10-10T12:03:00.000Z	\\xabababababababababababababababababababababababababababababababab	\N
\.
-- PostgreSQL database dump complete
