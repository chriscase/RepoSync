/** Cleanup planner. It never emits a global prune, and it refuses foreign resources. */

const FORBIDDEN = [
  "docker system prune",
  "docker volume prune",
  "docker network prune",
  "docker container prune",
  "docker image prune",
];

export function planCleanup({ runId, project, resources, commands }) {
  const errors = [];
  if (!runId || !/^[a-z0-9][a-z0-9_-]{4,40}$/.test(runId)) errors.push("run id is missing or unsafe");
  if (!project || !project.includes(runId)) errors.push("compose project does not contain the run id");
  for (const command of commands || []) {
    const text = String(command).toLowerCase();
    for (const forbidden of FORBIDDEN) {
      if (text.includes(forbidden)) errors.push(`forbidden command: ${forbidden}`);
    }
    if (text.includes("docker rm") || text.includes("docker volume rm") || text.includes("docker network rm")) {
      if (!text.includes(runId) && !text.includes("label=acceptance-lab.run-id")) {
        errors.push(`cleanup command is not bound to the run id: ${command}`);
      }
    }
  }
  const remove = [];
  for (const resource of resources || []) {
    const labels = resource.labels || {};
    const id = resource.id || "";
    if (!id) {
      errors.push("resource is missing an id");
      continue;
    }
    if (labels["acceptance-lab.run-id"] !== runId || labels["acceptance-lab.disposable"] !== "true") {
      errors.push(`refusing resource ${id}: labels do not match this disposable run`);
      continue;
    }
    if (resource.project && resource.project !== project) {
      errors.push(`refusing resource ${id}: project ${resource.project} is not ${project}`);
      continue;
    }
    remove.push(id);
  }
  return { ok: errors.length === 0, errors, remove };
}
