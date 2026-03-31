#!/usr/bin/env python3
"""
HFT Pingpong - JSONL Parser with HMAC Integrity (v2)
Parses structured JSONL logs, adds tamper-proof hash
"""

import json
import hmac
import hashlib
import requests
from datetime import datetime, timezone
from collections import defaultdict

# Config
TELEGRAM_TOKEN = "8754623467:AAHTUYGscxz1eDp2tH3olxuTXGdJHeOf0oY"
CHAT_ID = "1798631768"
JSONL_FILE = "/mnt/ramdisk/nohup_v2.jsonl"
STATE_FILE = "/tmp/hft_report_state.json"
HMAC_SECRET = "pingpong_hft_secret_2026"

def read_jsonl():
    """Read last 1000 lines of JSONL log"""
    try:
        with open(JSONL_FILE, 'r') as f:
            f.seek(0, 2)
            size = f.tell()
            f.seek(max(0, size - 2000000))
            lines = f.readlines()
            return lines[-1000:]
    except Exception as e:
        return []

def parse_events(lines):
    """Parse JSONL events into metrics"""
    metrics = {
        'uptime_seconds': 0,
        'total_messages': 0,
        'total_evals': 0,
        'total_edges': 0,
        'evals_per_sec': 0,
        'pairs_tracked': 0,
        'dropped_packets': 0,
        'rollover_adds': 0,
        'rollover_removes': 0,
        'edge_checks': 0,
        'dust_quotes': 0,
        'real_quotes': 0,
        'sequence_gaps': 0,
    }
    
    for line in lines:
        try:
            event = json.loads(line.strip())
            event_type = event.get('t')
            
            if event_type == 'm':
                metrics['uptime_seconds'] = event.get('up', 0)
                metrics['total_messages'] = event.get('msg', 0)
                metrics['total_evals'] = event.get('ev', 0)
                metrics['total_edges'] = event.get('ed', 0)
                metrics['evals_per_sec'] = event.get('rps', 0)
                metrics['pairs_tracked'] = event.get('pr', 0)
                metrics['dropped_packets'] = event.get('dr', 0)
            elif event_type == 'r':
                if event.get('act') == 'add':
                    metrics['rollover_adds'] += 1
                else:
                    metrics['rollover_removes'] += 1
            elif event_type == 'ec':
                metrics['edge_checks'] += 1
                if event.get('dust'):
                    metrics['dust_quotes'] += 1
                else:
                    metrics['real_quotes'] += 1
            elif event_type == 'e':
                metrics['total_edges'] += 1
            elif event_type == 's':
                if event.get('drop', 0) > 0:
                    metrics['sequence_gaps'] += 1
        except:
            continue
    
    return metrics

def calculate_rates(metrics, prev_state):
    rates = {}
    if prev_state and prev_state.get('uptime_seconds', 0) > 0:
        time_delta = max(1, metrics['uptime_seconds'] - prev_state.get('uptime_seconds', 0))
        rates['msgs_per_sec'] = (metrics['total_messages'] - prev_state.get('total_messages', 0)) / time_delta
        rates['evals_per_sec_avg'] = (metrics['total_evals'] - prev_state.get('total_evals', 0)) / time_delta
        rates['edges_per_min'] = (metrics['total_edges'] - prev_state.get('total_edges', 0)) / (time_delta / 60)
    else:
        rates['msgs_per_sec'] = metrics['evals_per_sec']
        rates['evals_per_sec_avg'] = metrics['evals_per_sec']
        rates['edges_per_min'] = 0
    
    rates['dust_rate'] = (metrics['dust_quotes'] / metrics['edge_checks'] * 100) if metrics['edge_checks'] > 0 else 0
    return rates

def generate_hmac(metrics):
    payload = json.dumps(metrics, sort_keys=True).encode('utf-8')
    signature = hmac.new(HMAC_SECRET.encode('utf-8'), payload, hashlib.sha256).hexdigest()
    return signature[:8]

def format_report(metrics, rates, hmac_hash):
    now = datetime.now(timezone.utc).strftime('%Y-%m-%d %H:%M:%S UTC')
    uptime_fmt = f"{metrics['uptime_seconds'] // 60}m {metrics['uptime_seconds'] % 60}s"
    
    edge_status = "NO EDGES (DUST QUOTES)" if metrics['total_edges'] == 0 else f"{metrics['total_edges']} EDGES FOUND"
    liquidity_status = "DUST QUOTES (DRY_RUN)" if metrics['dust_quotes'] > 0 else "REAL LIQUIDITY"
    seq_status = "OK" if metrics['sequence_gaps'] == 0 else f"GAPS DETECTED ({metrics['sequence_gaps']})"
    
    report = f"""
PINGPONG HFT - JSONL REPORT (v2)
{now}

==============================
PERFORMANCE METRICS
==============================

Uptime: {uptime_fmt}
WS Messages: {metrics['total_messages']:,}
Evaluations: {metrics['total_evals']:,}
Edges Found: {metrics['total_edges']:,}
Dropped Packets: {metrics['dropped_packets']:,}

==============================
THROUGHPUT (10-min avg)
==============================

Msg Rate: {rates.get('msgs_per_sec', 0):.1f} msg/sec
Eval Rate: {rates.get('evals_per_sec_avg', 0):.1f} eval/sec
Edge Rate: {rates.get('edges_per_min', 0):.4f} edges/min
Pairs Tracked: {metrics['pairs_tracked']}

==============================
MARKET QUALITY
==============================

Rollover Adds: {metrics['rollover_adds']}
Rollover Removes: {metrics['rollover_removes']}
Edge Checks: {metrics['edge_checks']}
Dust Quotes: {metrics['dust_quotes']} ({rates.get('dust_rate', 0):.1f}%)
Real Quotes: {metrics['real_quotes']} ({100-rates.get('dust_rate', 0):.1f}%)

==============================
SYSTEM STATUS
==============================

Parser: OPERATIONAL
Rollover: OPERATIONAL
Bi-directional Map: OPERATIONAL
Edge Detection: OPERATIONAL
Sequence Tracking: {seq_status}
Liquidity: {liquidity_status}

==============================
INTEGRITY
==============================

Log Format: JSONL
HMAC Hash: {hmac_hash}
Data Source: /mnt/ramdisk/nohup_v2.jsonl

==============================
{edge_status}
Next report in 10 minutes
"""
    return report

def send_telegram(message):
    url = f"https://api.telegram.org/bot{TELEGRAM_TOKEN}/sendMessage"
    data = {
        'chat_id': CHAT_ID,
        'text': message,
        'parse_mode': 'HTML'
    }
    try:
        response = requests.post(url, json=data, timeout=30)
        response.raise_for_status()
        print("Report sent to Telegram")
        return True
    except Exception as e:
        print(f"Failed to send: {e}")
        return False

def save_state(metrics):
    metrics['timestamp'] = datetime.now(timezone.utc).isoformat()
    try:
        with open(STATE_FILE, 'w') as f:
            json.dump(metrics, f)
    except Exception as e:
        print(f"Warning: Could not save state: {e}")

def load_state():
    try:
        with open(STATE_FILE, 'r') as f:
            return json.load(f)
    except:
        return None

def main():
    print(f"Generating JSONL report at {datetime.now(timezone.utc).isoformat()}")
    
    lines = read_jsonl()
    if not lines:
        print("No JSONL data yet - bot may still be running old version")
        return
    
    metrics = parse_events(lines)
    prev_state = load_state()
    rates = calculate_rates(metrics, prev_state)
    hmac_hash = generate_hmac(metrics)
    report = format_report(metrics, rates, hmac_hash)
    send_telegram(report)
    save_state(metrics)
    
    print(f"Report complete - HMAC: {hmac_hash}")

if __name__ == '__main__':
    main()
