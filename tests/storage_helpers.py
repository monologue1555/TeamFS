"""Decode stored payloads across supported formats; tests must not assume whole-file blobs."""

def content_bytes(conn, content_id):
    version = conn.execute("SELECT version FROM metadata").fetchone()[0]
    if version < 4:
        return conn.execute("SELECT data FROM blobs WHERE id=?", (content_id,)).fetchone()[0]
    size = conn.execute("SELECT size FROM contents WHERE id=?", (content_id,)).fetchone()[0]
    chunks = conn.execute("SELECT b.data FROM content_blocks c JOIN blobs b ON b.id=c.blob_id WHERE c.content_id=? ORDER BY c.page", (content_id,)).fetchall()
    data = b"".join(row[0] for row in chunks)
    assert len(data) == size
    return data


def stored_bytes(conn, name, scope=""):
    import os
    cid = conn.execute("SELECT content_id FROM nodes WHERE scope=? AND name=?", (scope, os.fsencode(name))).fetchone()[0]
    return content_bytes(conn, cid)


def legacy_nodes(conn):
    return [(scope, ino, parent, name, attr, content_bytes(conn, cid))
            for scope, ino, parent, name, attr, cid in conn.execute("SELECT scope,ino,parent,name,attr,content_id FROM nodes ORDER BY scope,ino").fetchall()]
