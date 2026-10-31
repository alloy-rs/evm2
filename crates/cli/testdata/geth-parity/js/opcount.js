{
    counts: {},
    step: function(log, db) {
        var op = log.op.toString();
        this.counts[op] = (this.counts[op] || 0) + 1;
    },
    fault: function(log, db) {},
    result: function(ctx, db) {
        return this.counts;
    }
}
