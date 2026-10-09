-- Derived OCR data is independent from original clipboard content and metadata.
CREATE TABLE IF NOT EXISTS image_ocr (
    item_id TEXT PRIMARY KEY NOT NULL REFERENCES clipboard_items(id) ON DELETE CASCADE,
    token TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending', 'completed', 'failed')),
    text TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_image_ocr_pending ON image_ocr(status, created_at);
CREATE VIRTUAL TABLE IF NOT EXISTS image_ocr_fts USING fts5(text, content='image_ocr', content_rowid='rowid', tokenize='trigram');
CREATE TRIGGER IF NOT EXISTS image_ocr_ai AFTER INSERT ON image_ocr BEGIN
    INSERT INTO image_ocr_fts(rowid, text) VALUES(new.rowid, new.text);
END;
CREATE TRIGGER IF NOT EXISTS image_ocr_ad AFTER DELETE ON image_ocr BEGIN
    INSERT INTO image_ocr_fts(image_ocr_fts, rowid, text) VALUES('delete', old.rowid, old.text);
END;
CREATE TRIGGER IF NOT EXISTS image_ocr_au AFTER UPDATE ON image_ocr BEGIN
    INSERT INTO image_ocr_fts(image_ocr_fts, rowid, text) VALUES('delete', old.rowid, old.text);
    INSERT INTO image_ocr_fts(rowid, text) VALUES(new.rowid, new.text);
END;

-- Rebuild also adopts a populated legacy table whose FTS index needs reconciliation.
INSERT INTO image_ocr_fts(image_ocr_fts) VALUES('rebuild');
