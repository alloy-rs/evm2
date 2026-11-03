{
    steps: [],
    faults: [],
    step: function(log, db) {
        var op = log.op.toString();
        if (op == "SSTORE" || op == "SLOAD" || op == "BALANCE" || op == "CALL" || op == "CREATE" || op == "LOG1" || op == "RETURN" || op == "REVERT") {
            var address = log.contract.getAddress();
            this.steps.push({
                pc: log.getPC(),
                op: op,
                depth: log.getDepth(),
                gas: log.getGas(),
                cost: log.getCost(),
                refund: log.getRefund(),
                memory: log.memory.length(),
                stack: log.stack.length(),
                top: log.stack.length() > 0 ? log.stack.peek(0).toString(16) : "none",
                address: toHex(address),
                caller: toHex(log.contract.getCaller()),
                value: log.contract.getValue().toString(),
                balance: db.getBalance(address).toString(),
                nonce: db.getNonce(address),
                code: db.getCode(address).length,
                exists: db.exists(address)
            });
        }
    },
    fault: function(log, db) {
        this.faults.push({pc: log.getPC(), op: log.op.toString(), depth: log.getDepth(), error: log.getError()});
    },
    result: function(ctx, db) {
        return {steps: this.steps, faults: this.faults};
    }
}
