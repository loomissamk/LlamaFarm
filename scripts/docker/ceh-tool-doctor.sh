#!/usr/bin/env bash
set -u

failures=0

check_path() {
  local command_name="$1"
  if command -v "$command_name" >/dev/null 2>&1; then
    printf 'PASS %-16s %s\n' "$command_name" "$(command -v "$command_name")"
  else
    printf 'FAIL %-16s missing\n' "$command_name"
    failures=$((failures + 1))
  fi
}

for command_name in \
  nmap tshark tcpdump nc dig whois traceroute ssh sshpass hydra sqlmap john \
  hashcat gobuster dirb aircrack-ng masscan nikto wifite searchsploit; do
  check_path "$command_name"
done

if command -v nikto >/dev/null 2>&1 && nikto -Version 2>&1 | grep -q '2\.6\.0'; then
  printf 'PASS %-16s pinned 2.6.0\n' 'nikto-runtime'
else
  printf 'FAIL %-16s version probe failed\n' 'nikto-runtime'
  failures=$((failures + 1))
fi

if command -v wifite >/dev/null 2>&1 && wifite --version 2>&1 | grep -Eqi 'wifite|2\.'; then
  printf 'PASS %-16s version probe succeeded\n' 'wifite-runtime'
else
  printf 'FAIL %-16s version probe failed\n' 'wifite-runtime'
  failures=$((failures + 1))
fi

if command -v searchsploit >/dev/null 2>&1 \
  && searchsploit --disable-colour apache 2>&1 | grep -qi 'exploit title'; then
  printf 'PASS %-16s local database query succeeded\n' 'searchsploit-db'
else
  printf 'FAIL %-16s local database query failed\n' 'searchsploit-db'
  failures=$((failures + 1))
fi

if command -v john >/dev/null 2>&1 \
  && john 2>&1 | grep -q 'John the Ripper password cracker'; then
  printf 'PASS %-16s runtime banner probe succeeded\n' 'john-runtime'
else
  printf 'FAIL %-16s runtime banner probe failed\n' 'john-runtime'
  failures=$((failures + 1))
fi

if command -v nvidia-smi >/dev/null 2>&1; then
  if hashcat -I 2>&1 | grep -Eq 'NVIDIA CUDA|NVIDIA Corporation'; then
    printf 'PASS %-16s NVIDIA OpenCL device discovered\n' 'hashcat-gpu'
  else
    printf 'FAIL %-16s NVIDIA GPU visible but Hashcat cannot discover it\n' 'hashcat-gpu'
    failures=$((failures + 1))
  fi
else
  printf 'INFO %-16s no NVIDIA runtime visible; CPU backend only\n' 'hashcat-gpu'
fi

if [ "$failures" -ne 0 ]; then
  printf '\nCEH doctor: %d failure(s)\n' "$failures"
  exit 1
fi

printf '\nCEH doctor: all required probes passed\n'
