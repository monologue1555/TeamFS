"""Archive raw sample/source bytes without Git newline normalization."""
import argparse
import hashlib
import json
from pathlib import Path
import zipfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run", type=Path)
    args = parser.parse_args()
    root = args.run.resolve()
    summary = json.loads((root / "summary.json").read_text(encoding="utf-8-sig"))
    if summary.get("status") != "pass":
        parser.error("only completed passing runs may be packaged for publication")
    archive = root / "raw-evidence.zip"
    manifest = root / "evidence-archive.json"
    if archive.exists() or manifest.exists():
        parser.error("archive already exists; historical evidence is never overwritten")
    files = sorted(p for p in root.rglob("*") if p.is_file())
    hashes = {p.relative_to(root).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest() for p in files}
    for relative, expected in summary["source"]["sha256"].items():
        if hashes.get("source/" + relative) != expected:
            parser.error(f"source snapshot hash mismatch: {relative}")
    for row in summary["samples"]:
        if row.get("status") != "pass":
            parser.error("failed sample in passing summary")
        prefix = row["evidence"]
        if json.loads((root / prefix / "sample.json").read_text()) != row:
            parser.error(f"sample and summary disagree: {prefix}")
        if hashes.get(prefix + "/report/outputs/result.json") != row["result_file_sha256"]:
            parser.error(f"output hash mismatch: {prefix}")
    with zipfile.ZipFile(archive, "x", compression=zipfile.ZIP_DEFLATED) as output:
        for path in files:
            if path.is_symlink():
                raise ValueError("symlink evidence is not supported")
            output.write(path, path.relative_to(root).as_posix())
    with zipfile.ZipFile(archive) as saved:
        for name, expected in hashes.items():
            if hashlib.sha256(saved.read(name)).hexdigest() != expected:
                raise ValueError(f"archive roundtrip mismatch: {name}")
    manifest.write_text(json.dumps({
        "archive": archive.name, "archive_sha256": hashlib.sha256(archive.read_bytes()).hexdigest(),
        "file_count": len(files), "files_sha256": hashes,
        "validation": "source snapshots, sample rows, output hashes and every archived byte verified",
        "extraction": "Extract into a new empty directory; the zip includes summary.json and all raw evidence.",
    }, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"Verified {len(files)} files; {archive.stat().st_size} bytes: {archive}")


if __name__ == "__main__":
    main()
