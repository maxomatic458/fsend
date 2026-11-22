CREATE TABLE transfers (
    hour TEXT NOT NULL,
    platform TEXT NOT NULL,
    transport TEXT NOT NULL,
    route TEXT NOT NULL,
    bytes INTEGER NOT NULL
);
CREATE INDEX transfers_hour ON transfers (hour);
