pub struct WeightedMeanInput {
    pub value_a: f64,
    pub weight_a: u64,
    pub value_b: f64,
    pub weight_b: u64,
}

pub struct EmaBatchInput {
    pub current: f64,
    pub alpha: f64,
    pub sample_mean: f64,
    pub count: u64,
}

pub struct ConvergenceDecayParams {
    pub now_unix_ms: u64,
    pub half_life_ms: u64,
    pub min_count: u64,
}

pub struct ConvergencePairRecord<'a> {
    pub loser: &'a str,
    pub winner: &'a str,
    pub count: u64,
}
