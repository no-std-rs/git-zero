"""Explicit upper bounds for individual benchmark observations."""
import json
import math


# Paths refer to existing raw report fields, without adding unrelated RSS peaks.
METRICS = {
    "client.wall_s": ("samples", "wall_s"),
    "client.cpu_s": ("samples", "cpu_s"),
    "client.max_rss_kib": ("samples", "max_rss_kib"),
    "server.wall_s": ("samples", "server", "wall_s"),
    "server.cpu_s": ("samples", "server", "cpu_s"),
    "server.frontend_peak_rss_kib": ("samples", "server", "frontend_peak_rss_kib"),
    "server.backend_child_peak_rss_kib": ("samples", "server", "backend_child_peak_rss_kib"),
    "startup.wall_s": ("server_startup", "startup_wall_s"),
    "startup.cpu_s": ("server_startup", "startup_cpu_s"),
    "startup.frontend_peak_rss_kib": ("server_startup", "frontend_peak_rss_kib"),
}


def nonnegative_number(value):
    return (type(value) is int and value >= 0
            or type(value) is float and math.isfinite(value) and value >= 0)


def load(path, workloads):
    """Reject misspelled, duplicate or unselected limits before starting servers."""
    policy = json.loads(path.read_text())
    if (not isinstance(policy, dict) or set(policy) != {"schema_version", "limits"}
            or type(policy["schema_version"]) is not int or policy["schema_version"] != 1
            or not isinstance(policy["limits"], list) or not policy["limits"]):
        raise ValueError("resource limits require schema_version 1 and a nonempty limits array")
    seen = set()
    for limit in policy["limits"]:
        if not isinstance(limit, dict) or set(limit) != {"workload", "implementation", "metric", "maximum"}:
            raise ValueError("each limit requires workload, implementation, metric and maximum")
        selector = tuple(limit[key] for key in ("workload", "implementation", "metric"))
        if not all(isinstance(value, str) for value in selector):
            raise ValueError("limit selectors must be strings")
        if selector[0] not in workloads or selector[1] not in ("git", "zero") or selector[2] not in METRICS:
            raise ValueError(f"unknown or unselected resource limit: {selector}")
        if not nonnegative_number(limit["maximum"]):
            raise ValueError("limit maximum must be a finite nonnegative number")
        if selector in seen:
            raise ValueError(f"duplicate resource limit: {selector}")
        seen.add(selector)
    return policy


def evaluate(policy, results):
    """Check the largest observation; missing/invalid observations never pass."""
    by_workload = {result["workload"]: result for result in results}
    evaluations = []
    for limit in policy["limits"]:
        result = by_workload.get(limit["workload"], {})
        group, *path = METRICS[limit["metric"]]
        observations = result.get(group, {}).get(limit["implementation"], [])
        if group == "server_startup":
            observations = [observations] if observations else []
        values = []
        for observation in observations:
            value = observation
            for key in path:
                value = value.get(key) if isinstance(value, dict) else None
            values.append(value)
        complete = bool(values) and all(nonnegative_number(value) for value in values)
        maximum = max(values) if complete else None
        status = ("missing_or_invalid" if not complete else
                  "passed" if maximum <= limit["maximum"] else "exceeded")
        evaluations.append({**limit, "observed_maximum": maximum, "observations": len(values), "status": status})
    return evaluations
