CREATE TABLE rh_topic_pages_v18 (
    page_slug TEXT NOT NULL PRIMARY KEY REFERENCES ros_wiki_topics (slug) ON DELETE CASCADE,
    topic_id TEXT UNIQUE REFERENCES rh_topics (id) ON DELETE SET NULL,
    sections TEXT NOT NULL DEFAULT '[]'
);
INSERT INTO rh_topic_pages_v18 (page_slug, topic_id, sections)
    SELECT page_slug, topic_id, sections FROM rh_topic_pages;
DROP TABLE rh_topic_pages;
CREATE TABLE rh_topic_pages (
    page_slug TEXT NOT NULL PRIMARY KEY REFERENCES ros_wiki_topics (slug) ON DELETE CASCADE,
    topic_id TEXT UNIQUE REFERENCES rh_topics (id) ON DELETE SET NULL,
    sections TEXT NOT NULL DEFAULT '[]'
);
INSERT INTO rh_topic_pages (page_slug, topic_id, sections)
    SELECT page_slug, topic_id, sections FROM rh_topic_pages_v18;
DROP TABLE rh_topic_pages_v18;
