CREATE TRIGGER IF NOT EXISTS lifecycle_events_track_terminal
AFTER INSERT ON lifecycle_events
WHEN NEW.event_type IN ('delivery.succeeded', 'delivery.failed')
BEGIN
    UPDATE emails
    SET terminal_event_type = NEW.event_type,
        terminal_event_at_ms = NEW.created_at,
        terminal_event_id = NEW.id
    WHERE id = NEW.email_id
      AND (terminal_event_at_ms IS NULL
           OR (NEW.created_at, NEW.id) > (terminal_event_at_ms, terminal_event_id));
END;

UPDATE emails
SET (terminal_event_type, terminal_event_at_ms, terminal_event_id) = (
    SELECT le.event_type, le.created_at, le.id
    FROM lifecycle_events le
    WHERE le.email_id = emails.id
      AND le.event_type IN ('delivery.succeeded', 'delivery.failed')
    ORDER BY le.created_at DESC, le.id DESC
    LIMIT 1
)
WHERE EXISTS (
    SELECT 1 FROM lifecycle_events le
    WHERE le.email_id = emails.id
      AND le.event_type IN ('delivery.succeeded', 'delivery.failed')
);
