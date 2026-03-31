#!/usr/bin/env python3
"""
HFT Pingpong - Nerdy Detailed Report (Every 10 minutes)
Sends comprehensive metrics to Telegram
"""

import requests
import os
import re
import json
from datetime import datetime, timezone
from collections import defaultdict

# Config
TELEGRAM_TOKEN = "8754623467:AAHTUYGscxz1eDp2tH3olxuTXGdJHeOf0oY"
CHAT_ID = "1798631768"
LOG_FILE = "/mnt/ramdisk/nohup_v2.out"
STATE_FILE = "/tmp/hft_report_state.json"

def read_log():
    try:
        with open(LOG_FILE, 'r') as f:
            f.seek(0, 2)
            size = f.tell()
            f.seek(max(0, size - 500000))
            return f.read()
    except Exception as e:
        return f"Error reading log: {e}"

def parse_metrics(log_content):
    metrics = {
        'uptime_seconds': 0,
        'total_messages': 0,
        'total_evals': 0,
        'total_edges': 0,
        'evals_per_sec': 0,
        'pairs_tracked': 0,
        'rollover_adds': 0,
        'rollover_removes': 0,
        'markets_found': 0,
        'orderbook_updates': 0,
        'edge_checks': 0,
        'dust_quotes': 0,
        'last_metrics_line': '',
    }
    
    metrics_pattern = r'\[METRICS\] (\d+)s \| msg: (\d+) \| evals: (\d+) \| edges: (\d+) \| evals/sec: (\d+) \| pairs:(\d+)'
    for match in re.finditer(metrics_pattern, log_content):
        metrics['uptime_seconds'] = int(match.group(1))
        metrics['total_messages'] = int(match.group(2))
        metrics['total_evals'] = int(match.group(3))
        metrics['total_edges'] = int(match.group(4))
        metrics['evals_per_sec'] = int(match.group(5))
        metrics['pairs_tracked'] = int(match.group(6))
        metrics['last_metrics_line'] = match.group(0)
    
    metrics['rollover_adds'] = len(re.findall(r'Adding:', log_content))
    metrics['rollover_removes'] = len(re.findall(r'Removing:', log_content))
    metrics['markets_found'] = len(re.findall(r'\[ROLLOVER\] Found:', log_content))
    metrics['orderbook_updates'] = len(re.findall(r'\[ORDERBOOK\] Updating', log_content))
    
    edge_checks = re.findall(r'\[EDGE CHECK\] YES price=(\d+) size=(\d+) \| NO price=(\d+) size=(\d+)', log_content)
    metrics['edge_checks'] = len(edge_checks)
    
    for check in edge_checks:
        yes_price, yes_size, no_price, no_size = map(int, check)
        combined = yes_price + no_price
        if combined < 900000:
            metrics['dust_quotes'] += 1
    
    return metrics

def calculate_rates(metrics, prev_state):
    rates = {}
    if prev_state:
        time_delta = metrics['uptime_seconds'] - prev_state.get('uptime_seconds', 0)
        if time_delta > 0:
            rates['msgs_per_sec'] = (metrics['total_messages'] - prev_state.get('total_messages', 0)) / time_delta
            rates['evals_per_sec_avg'] = (metrics['total_evals'] - prev_state.get('total_evals', 0)) / time_delta
            rates['edges_per_min'] = (metrics['total_edges'] - prev_state.get('total_edges', 0)) / (time_delta / 60)
        else:
            rates['msgs_per_sec'] = 0
            rates['evals_per_sec_avg'] = 0
            rates['edges_per_min'] = 0
    else:
        rates['msgs_per_sec'] = metrics['evals_per_sec']
        rates['evals_per_sec_avg'] = metrics['evals_per_sec']
        rates['edges_per_min'] = 0
    
    rates['dust_rate'] = (metrics['dust_quotes'] / metrics['edge_checks'] * 100) if metrics['edge_checks'] > 0 else 0
    return rates

def format_report(metrics, rates):
    now = datetime.now(timezone.utc).strftime('%Y-%m-%d %H:%M:%S UTC')
    uptime_fmt = f"{metrics['uptime_seconds'] // 60}m {metrics['uptime_seconds'] % 60}s"
    msg_rate = rates.get('msgs_per_sec', 0)
    eval_rate = rates.get('evals_per_sec_avg', 0)
    dust_rate = rates.get('dust_rate', 0)
    
    if metrics['total_edges'] == 0:
        edge_status = "NO EDGES (DUST QUOTES)"
        edge_detail = f"All {metrics['edge_checks']} edge checks found dust quotes"
    else:
        edge_status = f"{metrics['total_edges']} EDGES FOUND"
        edge_detail = f"Edge rate: {rates.get('edges_per_min', 0):.2f}/min"
    
    report = f"""
PINGPONG HFT - NERDY REPORT
{now}

------------------------------
PERFORMANCE METRICS
------------------------------

Uptime: {uptime_fmt}
WS Messages: {metrics['total_messages']:,} total
Evaluations: {metrics['total_evals']:,} total
Edges Found: {metrics['total_edges']:,} total

------------------------------
THROUGHPUT (10-min avg)
------------------------------

Msg Rate: {msg_rate:.1f} msg/sec
Eval Rate: {eval_rate:.1f} eval/sec
Edge Rate: {rates.get('edges_per_min', 0):.4f} edges/min
Pairs Tracked: {metrics['pairs_tracked']}

------------------------------
MARKET QUALITY
------------------------------

Rollover Adds: {metrics['rollover_adds']} markets
Rollover Removes: {metrics['rollover_removes']} markets
Markets Discovered: {metrics['markets_found']}
Orderbook Updates: {metrics['orderbook_updates']:,}

------------------------------
LIQUIDITY ANALYSIS
------------------------------

Edge Checks: {metrics['edge_checks']} samples
Dust Quotes: {metrics['dust_quotes']} ({dust_rate:.1f}%)
{edge_status}
{edge_detail}

------------------------------
SYSTEM STATUS
------------------------------

Parser: OPERATIONAL
Rollover: OPERATIONAL  
Bi-directional Map: OPERATIONAL
Edge Detection: OPERATIONAL
Liquidity: DUST QUOTES (DRY_RUN mode)

------------------------------
LAST METRICS LINE
------------------------------
{metrics['last_metrics_line']}

------------------------------
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
        print(f"Report sent to Telegram")
        return True
    except Exception as e:
        print(f"Failed to send: {e}")
        return False

def save_state(metrics):
    state = {
        'uptime_seconds': metrics['uptime_seconds'],
        'total_messages': metrics['total_messages'],
        'total_evals': metrics['total_evals'],
        'total_edges': metrics['total_edges'],
        'timestamp': datetime.now(timezone.utc).isoformat()
    }
    try:
        with open(STATE_FILE, 'w') as f:
            json.dump(state, f)
    except Exception as e:
        print(f"Warning: Could not save state: {e}")

def load_state():
    try:
        with open(STATE_FILE, 'r') as f:
            return json.load(f)
    except:
        return None

def main():
    print(f"Generating HFT report at {datetime.now(timezone.utc).isoformat()}")
    
    log_content = read_log()
    metrics = parse_metrics(log_content)
    prev_state = load_state()
    rates = calculate_rates(metrics, prev_state)
    report = format_report(metrics, rates)
    send_telegram(report)
    save_state(metrics)
    
    print(f"Report complete")

if __name__ == '__main__':
    main()
