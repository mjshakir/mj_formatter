from __future__ import annotations

import os
import tempfile
from pathlib import Path


class AtomicWriter:
    @staticmethod
    def write_bytes(path: Path, data: bytes) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        temp_path: Path | None = None
        try:
            with tempfile.NamedTemporaryFile("wb", delete=False, dir=str(path.parent)) as handle:
                handle.write(data)
                handle.flush()
                os.fsync(handle.fileno())
                temp_path = Path(handle.name)
            os.replace(str(temp_path), str(path))
            AtomicWriter._fsync_dir(path.parent)
        except Exception:
            if temp_path is not None:
                try:
                    temp_path.unlink(missing_ok=True)
                except Exception:
                    pass
            raise

    @staticmethod
    def write_text(path: Path, text: str, encoding: str = "utf-8") -> None:
        AtomicWriter.write_bytes(path, text.encode(encoding))

    @staticmethod
    def _fsync_dir(directory: Path) -> None:
        try:
            fd = os.open(str(directory), os.O_RDONLY)
        except Exception:
            return
        try:
            os.fsync(fd)
        except Exception:
            return
        finally:
            try:
                os.close(fd)
            except Exception:
                pass

