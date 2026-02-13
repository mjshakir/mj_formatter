from __future__ import annotations

import logging

from ..types import SummaryContext


class SummaryLogger:
    def log_and_status(self, logger: logging.Logger, context: SummaryContext) -> int:
        files = 0
        changed = 0
        errors = 0
        violations = 0
        cache_hits = 0
        warnings_count = 0
        conflict_count = 0
        policy_counts: dict[str, int] = {}
        policy_times: dict[str, float] = {}

        for result in context.results:
            files += 1
            if result.changed:
                changed += 1
            if result.error:
                errors += 1
            violations += len(result.violations)
            if result.cache_hit:
                cache_hits += 1
            warnings_count += len(result.warnings or [])
            for violation in result.violations:
                policy_counts[violation.policy] = policy_counts.get(violation.policy, 0) + 1
                if violation.policy == "policy_conflict_detector":
                    conflict_count += 1
            if result.profile:
                for name, ms in result.profile.items():
                    policy_times[name] = policy_times.get(name, 0.0) + float(ms)
            if context.verbose and result.violations:
                logger.info("violations in %s (%d)", result.path, len(result.violations))
                for violation in result.violations:
                    line = f"  - {violation.policy}: {violation.message} (line {violation.line}"
                    if violation.column is not None:
                        line += f", col {violation.column}"
                    line += ")"
                    logger.info("%s", line)
            if context.verbose and result.warnings:
                logger.info("warnings in %s (%d)", result.path, len(result.warnings))
                for message in result.warnings:
                    logger.info("  - parser: %s", message)

        logger.info("files processed: %s", files)
        logger.info("files changed: %s", changed)
        logger.info("violations: %s", violations)
        logger.info("errors: %s", errors)
        logger.info("cache hits: %s", cache_hits)
        logger.info("warnings: %s", warnings_count)
        logger.info("conflicts: %s", conflict_count)
        logger.info("jobs used: %s", context.jobs)
        logger.info("elapsed: %.3fs", context.elapsed)
        if context.elapsed > 0:
            logger.info("throughput: %.2f files/s", files / context.elapsed)
        if policy_counts:
            top = sorted(policy_counts.items(), key=lambda item: item[1], reverse=True)[:5]
            logger.info("top policies: %s", ", ".join(f"{name}={count}" for name, count in top))
        if policy_times:
            top_time = sorted(policy_times.items(), key=lambda item: item[1], reverse=True)[:5]
            logger.info(
                "top policy times (ms): %s",
                ", ".join(f"{name}={ms:.2f}" for name, ms in top_time),
            )

        if errors:
            return 2
        if context.fail_on_conflict and conflict_count > 0:
            return 2
        if context.check_only and violations:
            return 1
        return 0
