pub mod asof_join;
pub mod kyles_lambda;
pub mod markout;
pub mod parkinson;
pub mod time_bucket;
pub mod twap;

use std::sync::Arc;
use datafusion::prelude::SessionContext;

use self::kyles_lambda::KylesLambdaSlopeUdaf;
use self::markout::{MarketMidTape, MarkoutPnlUdaf};
use self::parkinson::RealizedVolatilityParkinsonUdaf;
use self::time_bucket::TimeBucketNsUdf;
use self::twap::TwapPreciseUdaf;

/// Registers all native Hyperion quantitative microstructure UDFs & UDAFs onto `SessionContext`.
pub fn register_financial_udfs(ctx: &SessionContext, mid_tape: Arc<MarketMidTape>) {
    ctx.register_udf(TimeBucketNsUdf::into_scalar_udf());
    ctx.register_udaf(TwapPreciseUdaf::into_aggregate_udf());
    ctx.register_udaf(RealizedVolatilityParkinsonUdaf::into_aggregate_udf());
    ctx.register_udaf(KylesLambdaSlopeUdaf::into_aggregate_udf());
    ctx.register_udaf(MarkoutPnlUdaf::into_aggregate_udf(mid_tape));
}
