{
    frames: [],
    enter: function(frame) {
        this.frames.push({
            type: frame.getType(),
            from: toHex(frame.getFrom()),
            to: toHex(frame.getTo()),
            input: toHex(frame.getInput()),
            gas: frame.getGas(),
            value: frame.getValue() === undefined ? "none" : frame.getValue().toString()
        });
    },
    exit: function(res) {
        this.frames.push({
            gasUsed: res.getGasUsed(),
            output: toHex(res.getOutput()),
            error: res.getError() === undefined ? "none" : res.getError()
        });
    },
    step: function(log, db) {},
    fault: function(log, db) {},
    result: function(ctx, db) {
        return {
            type: ctx.type,
            from: toHex(ctx.from),
            to: toHex(ctx.to),
            input: toHex(ctx.input),
            gas: ctx.gas,
            gasUsed: ctx.gasUsed,
            gasPrice: ctx.gasPrice,
            value: ctx.value.toString(),
            output: toHex(ctx.output),
            error: ctx.error === undefined ? "none" : ctx.error,
            frames: this.frames
        };
    }
}
