ALTER TABLE emails ADD COLUMN enqueue_state TEXT NOT NULL DEFAULT 'enqueued';
