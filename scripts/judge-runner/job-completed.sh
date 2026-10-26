#!/bin/bash
# The judging runner's job-completed hook (ADR 0052, docs/design/ci.md
# 「判定机器」). An administrator installs it root-owned as
# /opt/lectern-judge/hooks/job-completed.sh, and the runner's .env names it in
# ACTIONS_RUNNER_HOOK_JOB_COMPLETED. The runner runs it as the dedicated user
# after the last step of every job, also of a failed or cancelled one,
# whatever the workflow says.
#
# It ends every process of this user that started after this job's
# Runner.Worker. It chooses them by the start times that the kernel keeps,
# which no job can change; not by name, path or a list that a job could edit.
# It keeps its own ancestors (the runner) and its own children.
#
# It fails closed. When it cannot find the worker among its ancestors or read
# the worker's start time, it ends every process of this user outside its
# ancestors and children, which ends the login session too, and exits with 1,
# so the job fails. It also exits with 1 when processes are still left after
# five rounds, or when it cannot list the processes.

set -u
PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH

me=$$
uid=$(id -u) || exit 1
status=0

# Input: "PID PPID ELAPSED NAME" per process. Output: "worker <pid>" or
# "worker none", then "end <pid> <name>" for each process to end.
program='
function seconds(t,   days, part, parts, i, s) {
  if (t !~ /^([0-9]+-)?([0-9]+:)?[0-9]+:[0-9]+$/) return -1
  days = 0
  if (index(t, "-") > 0) {
    days = substr(t, 1, index(t, "-") - 1)
    t = substr(t, index(t, "-") + 1)
  }
  parts = split(t, part, ":")
  s = 0
  for (i = 1; i <= parts; i++) s = s * 60 + part[i]
  return days * 86400 + s
}
{ parent[$1] = $2; age[$1] = seconds($3); name[$1] = $4; pids[++n] = $1 }
END {
  worker = ""
  for (q = me; (q in parent) && h++ < 64; q = parent[q]) {
    keep[q]
    if (worker == "" && name[q] == "Runner.Worker" && age[q] >= 0) worker = q
  }
  print "worker " (worker == "" ? "none" : worker)
  for (i = 1; i <= n; i++) {
    p = pids[i]
    if (p in keep) continue
    mine = 0; h = 0
    for (q = parent[p]; (q in parent) && h++ < 64; q = parent[q])
      if (q == me) { mine = 1; break }
    if (mine) continue
    if (worker == "" || age[p] < 0 || age[p] <= age[worker]) print "end " p " " name[p]
  }
}'

for round in 1 2 3 4 5; do
  table=$(ps -U "$uid" -o pid=,ppid=,etime=,ucomm=) || {
    echo "::error::Cannot list the processes of user $uid"
    exit 1
  }
  result=$(printf '%s\n' "$table" | awk -v me="$me" "$program") || {
    echo "::error::Cannot read the processes of user $uid"
    exit 1
  }
  worker=$(printf '%s\n' "$result" | sed -n 's/^worker //p')
  ends=$(printf '%s\n' "$result" | sed -n 's/^end //p')
  if [ "$worker" = none ] && [ "$status" = 0 ]; then
    echo "::error::No Runner.Worker with a readable start time among the ancestors: ending every process of user $uid outside the runner (fail closed)"
    status=1
  fi
  if [ -z "$ends" ]; then
    exit "$status"
  fi
  echo "Round $round ends these processes (PID, name):"
  printf '%s\n' "$ends" | sed 's/^/  /'
  # shellcheck disable=SC2046 # one PID per word
  kill -9 $(printf '%s\n' "$ends" | awk '{ print $1 }') 2>/dev/null || true
  sleep 1
done
echo "::error::Processes of user $uid are still running after five rounds"
exit 1
