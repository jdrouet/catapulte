CREATE OR REPLACE FUNCTION lifecycle_events_track_terminal() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    UPDATE emails
    SET terminal_event_type = NEW.event_type,
        terminal_event_at_ms = NEW.created_at,
        terminal_event_id = NEW.id
    WHERE id = NEW.email_id
      AND (terminal_event_at_ms IS NULL
           OR (NEW.created_at, NEW.id) > (terminal_event_at_ms, terminal_event_id));
    RETURN NULL;
END;
$$;

DROP TRIGGER IF EXISTS lifecycle_events_track_terminal ON lifecycle_events;
CREATE TRIGGER lifecycle_events_track_terminal
AFTER INSERT ON lifecycle_events
FOR EACH ROW
WHEN (NEW.event_type IN ('delivery.succeeded', 'delivery.failed'))
EXECUTE FUNCTION lifecycle_events_track_terminal();

UPDATE emails e
SET terminal_event_type = t.event_type,
    terminal_event_at_ms = t.created_at,
    terminal_event_id = t.id
FROM (
    SELECT DISTINCT ON (email_id) email_id, event_type, created_at, id
    FROM lifecycle_events
    WHERE event_type IN ('delivery.succeeded', 'delivery.failed')
    ORDER BY email_id, created_at DESC, id DESC
) t
WHERE e.id = t.email_id;
