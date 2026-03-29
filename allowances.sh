#!/bin/bash
# Token Allowances for Polymarket Live Trading
# Run ONCE after funding your wallet with USDC.e on Polygon
# 
# Prerequisites:
#   - Foundry installed (https://book.getfoundry.sh/)
#   - USDC.e in your wallet
#   - Private key exported as PK

set -e

export RPC="https://polygon-rpc.com"

echo "🔑 Checking private key..."
if [ -z "$PK" ]; then
    echo "❌ Error: PK environment variable not set"
    echo "Usage: PK=your_private_key ./allowances.sh"
    exit 1
fi

echo ""
echo "📋 Approving USDC.e for trading..."

# 1. Approve USDC.e for Main Exchange
echo "1/6: Main Exchange..."
cast send 0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174 \
    "approve(address,uint256)" \
    0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E \
    115792089237316195423570985008687907853269984665640564039457584007913129639935 \
    --private-key $PK --rpc-url $RPC

# 2. Approve USDC.e for Negative Risk Exchange
echo "2/6: Negative Risk Exchange..."
cast send 0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174 \
    "approve(address,uint256)" \
    0xC5d563A36AE78145C45a50134d48A1215220f80a \
    115792089237316195423570985008687907853269984665640564039457584007913129639935 \
    --private-key $PK --rpc-url $RPC

# 3. Approve USDC.e for Negative Risk Adapter
echo "3/6: Negative Risk Adapter..."
cast send 0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174 \
    "approve(address,uint256)" \
    0xd91E80cF2E7be2e162c6513ceD06f1dD0dA35296 \
    115792089237316195423570985008687907853269984665640564039457584007913129639935 \
    --private-key $PK --rpc-url $RPC

echo ""
echo "📋 Setting CTF token approvals..."

# 4. SetApprovalForAll CTF for Main Exchange
echo "4/6: CTF Main Exchange..."
cast send 0x4D97DCd97eC945f40cF65F87097ACe5EA0476045 \
    "setApprovalForAll(address,bool)" \
    0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E \
    true \
    --private-key $PK --rpc-url $RPC

# 5. SetApprovalForAll CTF for Negative Risk Exchange
echo "5/6: CTF Negative Risk Exchange..."
cast send 0x4D97DCd97eC945f40cF65F87097ACe5EA0476045 \
    "setApprovalForAll(address,bool)" \
    0xC5d563A36AE78145C45a50134d48A1215220f80a \
    true \
    --private-key $PK --rpc-url $RPC

# 6. SetApprovalForAll CTF for Negative Risk Adapter
echo "6/6: CTF Negative Risk Adapter..."
cast send 0x4D97DCd97eC945f40cF65F87097ACe5EA0476045 \
    "setApprovalForAll(address,bool)" \
    0xd91E80cF2E7be2e162c6513ceD06f1dD0dA35296 \
    true \
    --private-key $PK --rpc-url $RPC

echo ""
echo "✅ All allowances set!"
echo ""
echo "📝 Summary:"
echo "   USDC.e approved for: Main Exchange, Neg Risk Exchange, Neg Risk Adapter"
echo "   CTF tokens approved for: Main Exchange, Neg Risk Exchange, Neg Risk Adapter"
echo ""
echo "⚠️  NOTE: These approvals are PERMANENT (uint256 max)."
echo "   To revoke, run: cast send <token> 'approve(address,uint256)' <spender> 0"