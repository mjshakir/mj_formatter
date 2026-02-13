from __future__ import annotations

from mj_formatter.core.runtime.worker_runner import WorkerRunner


def test_iter_batches_preserves_order_and_size() -> None:
    paths = [f"file_{index}.cpp" for index in range(7)]
    batches = list(WorkerRunner._iter_batches(paths, 3))
    assert batches == [
        ["file_0.cpp", "file_1.cpp", "file_2.cpp"],
        ["file_3.cpp", "file_4.cpp", "file_5.cpp"],
        ["file_6.cpp"],
    ]


def test_iter_batches_clamps_small_size() -> None:
    paths = ["a.cpp", "b.cpp"]
    batches = list(WorkerRunner._iter_batches(paths, 0))
    assert batches == [["a.cpp"], ["b.cpp"]]


def test_build_batches_smart_balances_by_file_size(monkeypatch) -> None:
    paths = ["a.cpp", "b.cpp", "c.cpp", "d.cpp"]
    sizes = {"a.cpp": 100, "b.cpp": 90, "c.cpp": 10, "d.cpp": 5}

    monkeypatch.setattr(
        WorkerRunner,
        "_file_size_hint",
        staticmethod(lambda path: sizes[path]),
    )

    batches = WorkerRunner._build_batches(paths, 2, True)
    assert len(batches) == 2
    assert sorted(path for batch in batches for path in batch) == sorted(paths)

    loads = [sum(sizes[path] for path in batch) for batch in batches]
    assert max(loads) - min(loads) <= 10


def test_build_batches_can_disable_smart_mode() -> None:
    paths = ["a.cpp", "b.cpp", "c.cpp"]
    batches = WorkerRunner._build_batches(paths, 2, False)
    assert batches == [["a.cpp", "b.cpp"], ["c.cpp"]]
