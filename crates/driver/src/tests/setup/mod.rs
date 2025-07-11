//! Framework for setting up tests.

use {
    self::{driver::Driver, solver::Solver as SolverInstance},
    crate::{
        domain::{
            competition::{order, order::app_data::AppData},
            eth,
            time,
        },
        infra::{
            self,
            config::file::{
                FeeHandler,
                OrderPriorityStrategy,
                default_http_time_buffer,
                default_solving_share_of_deadline,
            },
        },
        tests::{
            cases::{
                AB_ORDER_AMOUNT,
                AD_ORDER_AMOUNT,
                CD_ORDER_AMOUNT,
                DEFAULT_POOL_AMOUNT_A,
                DEFAULT_POOL_AMOUNT_B,
                DEFAULT_POOL_AMOUNT_C,
                DEFAULT_POOL_AMOUNT_D,
                DEFAULT_SURPLUS_FACTOR,
                ETH_ORDER_AMOUNT,
                EtherExt,
                is_approximately_equal,
            },
            hex_address,
            setup::{
                blockchain::{Blockchain, Interaction, Trade},
                orderbook::Orderbook,
            },
        },
    },
    bigdecimal::{BigDecimal, FromPrimitive},
    ethcontract::dyns::DynTransport,
    futures::future::join_all,
    hyper::StatusCode,
    model::order::{BuyTokenDestination, SellTokenSource},
    number::serialization::HexOrDecimalU256,
    primitive_types::H160,
    secp256k1::SecretKey,
    serde_with::serde_as,
    solvers_dto::solution::Flashloan,
    std::{
        collections::{HashMap, HashSet},
        path::PathBuf,
        str::FromStr,
    },
};

pub mod blockchain;
mod driver;
pub mod fee;
mod orderbook;
mod solver;

#[derive(Debug, Clone, Copy)]
pub struct Asset {
    token: &'static str,
    amount: eth::U256,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Partial {
    #[default]
    No,
    Yes {
        executed: eth::U256,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiquidityQuote {
    pub sell_token: &'static str,
    pub buy_token: &'static str,
    pub sell_amount: eth::U256,
    pub buy_amount: eth::U256,
}

impl LiquidityQuote {
    pub fn buy_amount(self, buy_amount: eth::U256) -> Self {
        Self { buy_amount, ..self }
    }

    pub fn sell_amount(self, sell_amount: eth::U256) -> Self {
        Self {
            sell_amount,
            ..self
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct JitOrder {
    pub order: Order,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Order {
    pub name: &'static str,

    pub sell_amount: eth::U256,
    /// The explicit limit buy amount for the order. If None, it will be derived
    /// from the order's quote.
    pub buy_amount: Option<eth::U256>,
    pub sell_token: &'static str,
    pub buy_token: &'static str,

    pub internalize: bool,
    pub side: order::Side,
    pub partial: Partial,
    pub created: u32,
    pub valid_to: u32,
    pub kind: order::Kind,

    // Currently used for limit orders to represent the surplus_fee calculated by the solver.
    pub solver_fee: Option<eth::U256>,

    /// Set a value to be used to divide the order buy or sell amount before
    /// the order gets placed and thereby generate surplus. Whether the sell or
    /// buy amount is divided depends on the order side. This is necessary to
    /// keep the solution scores positive.
    pub surplus_factor: eth::U256,
    /// Override the executed target amount of the order. Useful for testing
    /// liquidity orders. Otherwise [`execution_diff`] is probably more
    /// suitable.
    pub executed: Option<eth::U256>,
    /// Provides explicit expected order executed amounts.
    pub expected_amounts: Option<ExpectedOrderAmounts>,
    /// Should this order be filtered out before being sent to the solver?
    pub filtered: bool,
    /// Should the trader account be funded with enough tokens to place this
    /// order? True by default.
    pub funded: bool,
    pub fee_policy: Vec<fee::Policy>,
    pub owner: H160,
    pub receiver: Option<H160>,
    pub fee_amount: eth::U256,
    pub sell_token_source: SellTokenSource,
    pub buy_token_destination: BuyTokenDestination,
    pub app_data: AppData,
    pub quote: Option<OrderQuote>,
    pub pre_interactions: Vec<Interaction>,
}

impl Order {
    /// Rename the order.
    pub fn rename(self, name: &'static str) -> Self {
        Self { name, ..self }
    }

    /// Reduce the sell amount of this order by the given amount.
    pub fn reduce_amount(self, diff: eth::U256) -> Self {
        Self {
            sell_amount: self.sell_amount - diff,
            ..self
        }
    }

    /// Multiply the sell amount of this order by the given factor.
    pub fn multiply_amount(self, mult: eth::U256) -> Self {
        Self {
            sell_amount: self.sell_amount * mult,
            ..self
        }
    }

    /// Ensure that this order generates no surplus, and therefore most likely
    /// has a negative score.
    pub fn no_surplus(self) -> Self {
        Self {
            surplus_factor: 1.into(),
            ..self
        }
    }

    /// Mark this order as internalizable.
    pub fn internalize(self) -> Self {
        Self {
            internalize: true,
            ..self
        }
    }

    /// Set the order kind.
    pub fn kind(self, kind: order::Kind) -> Self {
        Self { kind, ..self }
    }

    /// Set the order side.
    pub fn side(self, side: order::Side) -> Self {
        Self { side, ..self }
    }

    /// Set the solver fee.
    pub fn solver_fee(self, solver_fee: Option<eth::U256>) -> Self {
        Self { solver_fee, ..self }
    }

    /// Make this a limit order.
    pub fn limit(self) -> Self {
        Self {
            kind: order::Kind::Limit,
            ..self
        }
    }

    /// Mark that this order should be filtered out before being sent to the
    /// solver.
    pub fn filtered(self) -> Self {
        Self {
            filtered: true,
            ..self
        }
    }

    /// Mark that the trader should not be funded with tokens that are needed to
    /// place this order.
    pub fn unfunded(self) -> Self {
        Self {
            funded: false,
            ..self
        }
    }

    pub fn fee_policy(self, fee_policy: Vec<fee::Policy>) -> Self {
        Self { fee_policy, ..self }
    }

    pub fn expected_amounts(self, expected_amounts: ExpectedOrderAmounts) -> Self {
        Self {
            expected_amounts: Some(expected_amounts),
            ..self
        }
    }

    pub fn sell_amount(self, sell_amount: eth::U256) -> Self {
        Self {
            sell_amount,
            ..self
        }
    }

    pub fn buy_amount(self, buy_amount: eth::U256) -> Self {
        Self {
            buy_amount: Some(buy_amount),
            ..self
        }
    }

    pub fn partial(self, already_executed: eth::U256) -> Self {
        Self {
            partial: Partial::Yes {
                executed: already_executed,
            },
            ..self
        }
    }

    pub fn executed(self, executed: Option<eth::U256>) -> Self {
        Self { executed, ..self }
    }

    pub fn created(self, created: u32) -> Self {
        Self { created, ..self }
    }

    pub fn valid_to(self, valid_to: u32) -> Self {
        Self { valid_to, ..self }
    }

    pub fn quote(self, quote: OrderQuote) -> Self {
        Self {
            quote: Some(quote),
            ..self
        }
    }

    pub fn pre_interaction(mut self, interaction: Interaction) -> Self {
        self.pre_interactions.push(interaction);
        self
    }

    pub fn app_data(self, app_data: AppData) -> Self {
        Self { app_data, ..self }
    }

    fn surplus_fee(&self) -> eth::U256 {
        match self.kind {
            order::Kind::Limit => self.solver_fee.unwrap_or_default(),
            _ => 0.into(),
        }
    }

    pub fn receiver(self, receiver: Option<H160>) -> Self {
        Self { receiver, ..self }
    }
}

impl Default for Order {
    fn default() -> Self {
        Self {
            sell_amount: Default::default(),
            buy_amount: None,
            sell_token: Default::default(),
            buy_token: Default::default(),
            internalize: Default::default(),
            side: order::Side::Sell,
            partial: Default::default(),
            created: u32::MIN,
            valid_to: u32::MAX,
            kind: order::Kind::Market,
            solver_fee: Default::default(),
            name: Default::default(),
            surplus_factor: DEFAULT_SURPLUS_FACTOR.ether().into_wei(),
            executed: Default::default(),
            expected_amounts: Default::default(),
            filtered: Default::default(),
            funded: true,
            fee_policy: vec![fee::Policy::Surplus {
                factor: 0.0,
                max_volume_factor: 0.06,
            }],
            owner: eth::H160::from_str(TRADER_ADDRESS).unwrap(),
            receiver: Default::default(),
            fee_amount: Default::default(),
            sell_token_source: Default::default(),
            buy_token_destination: Default::default(),
            app_data: Default::default(),
            quote: Default::default(),
            pre_interactions: Default::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Solver {
    /// A human readable identifier of the solver
    name: String,
    /// How much ETH balance should the solver be funded with? 1 ETH by default.
    balance: eth::U256,
    /// The private key for this solver.
    private_key: ethcontract::PrivateKey,
    /// The slippage for this solver.
    slippage: Slippage,
    /// The fraction of time used for solving
    timeouts: infra::solver::Timeouts,
    /// Determines whether the `solver` or the `driver` handles the fees
    fee_handler: FeeHandler,
    /// Whether or not solver is allowed to combine multiple solutions into a
    /// new one.
    merge_solutions: bool,
}

#[derive(Debug, Clone)]
struct Slippage {
    relative: BigDecimal,
    absolute: Option<eth::Ether>,
}

pub fn test_solver() -> Solver {
    Solver {
        name: solver::NAME.to_owned(),
        balance: eth::U256::exp10(18),
        private_key: ethcontract::PrivateKey::from_slice(
            hex::decode("a131a35fb8f614b31611f4fe68b6fc538b0febd2f75cd68e1282d8fd45b63326")
                .unwrap(),
        )
        .unwrap(),
        slippage: Slippage {
            relative: BigDecimal::from_f64(0.3).unwrap(),
            absolute: Some(183.into()),
        },
        timeouts: infra::solver::Timeouts {
            http_delay: chrono::Duration::from_std(default_http_time_buffer()).unwrap(),
            solving_share_of_deadline: default_solving_share_of_deadline().try_into().unwrap(),
        },
        fee_handler: FeeHandler::default(),
        merge_solutions: false,
    }
}

impl Solver {
    pub fn address(&self) -> eth::H160 {
        self.private_key.public_address()
    }

    pub fn name(self, name: &str) -> Self {
        Self {
            name: name.to_owned(),
            ..self
        }
    }

    pub fn solving_time_share(self, share: f64) -> Self {
        Self {
            timeouts: infra::solver::Timeouts {
                solving_share_of_deadline: share.try_into().unwrap(),
                ..self.timeouts
            },
            ..self
        }
    }

    pub fn balance(self, balance: eth::U256) -> Self {
        Self { balance, ..self }
    }

    pub fn fee_handler(mut self, fee_handler: FeeHandler) -> Self {
        self.fee_handler = fee_handler;
        self
    }

    pub fn merge_solutions(mut self) -> Self {
        self.merge_solutions = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pool {
    pub token_a: &'static str,
    pub token_b: &'static str,
    pub amount_a: eth::U256,
    pub amount_b: eth::U256,
}

impl Pool {
    /// Restores reserve_a value from the given reserve_b and the quote. Reverse
    /// operation for the `blockchain::Pool::out` function.
    /// <https://en.wikipedia.org/wiki/Floor_and_ceiling_functions>
    #[allow(dead_code)]
    pub fn adjusted_reserve_a(self, quote: &LiquidityQuote) -> Self {
        let (quote_sell_amount, quote_buy_amount) = if quote.sell_token == self.token_a {
            (quote.sell_amount, quote.buy_amount)
        } else {
            (quote.buy_amount, quote.sell_amount)
        };
        let reserve_a_min = ceil_div(
            eth::U256::from(997)
                * quote_sell_amount
                * (self.amount_b - quote_buy_amount - eth::U256::from(1)),
            eth::U256::from(1000) * quote_buy_amount,
        );
        let reserve_a_max =
            (eth::U256::from(997) * quote_sell_amount * (self.amount_b - quote_buy_amount))
                / (eth::U256::from(1000) * quote_buy_amount);
        if reserve_a_min > reserve_a_max {
            panic!(
                "Unexpected calculated reserves. min: {reserve_a_min:?}, max: {reserve_a_max:?}"
            );
        }
        Self {
            amount_a: reserve_a_min,
            ..self
        }
    }

    /// Restores reserve_b value from the given reserve_a and the quote. Reverse
    /// operation for the `blockchain::Pool::out` function
    /// <https://en.wikipedia.org/wiki/Floor_and_ceiling_functions>
    pub fn adjusted_reserve_b(self, quote: &LiquidityQuote) -> Self {
        let (quote_sell_amount, quote_buy_amount) = if quote.sell_token == self.token_a {
            (quote.sell_amount, quote.buy_amount)
        } else {
            (quote.buy_amount, quote.sell_amount)
        };
        let reserve_b_min = ceil_div(
            quote_buy_amount
                * (eth::U256::from(1000) * self.amount_a
                    + eth::U256::from(997) * quote_sell_amount),
            eth::U256::from(997) * quote_sell_amount,
        );
        let reserve_b_max = ((quote_buy_amount + eth::U256::from(1))
            * (eth::U256::from(1000) * self.amount_a + eth::U256::from(997) * quote_sell_amount)
            - eth::U256::from(1))
            / (eth::U256::from(997) * quote_sell_amount);
        if reserve_b_min > reserve_b_max {
            panic!(
                "Unexpected calculated reserves. min: {reserve_b_min:?}, max: {reserve_b_max:?}"
            );
        }
        Self {
            amount_b: reserve_b_min,
            ..self
        }
    }
}

fn ceil_div(x: eth::U256, y: eth::U256) -> eth::U256 {
    (x + y - eth::U256::from(1)) / y
}

#[derive(Debug)]
pub enum Mempool {
    Public,
    Private {
        /// Uses ethrpc node if None
        url: Option<String>,
    },
}

/// Create a builder for the setup process.
pub fn setup() -> Setup {
    Setup {
        solvers: vec![test_solver()],
        enable_simulation: true,
        mempools: vec![Mempool::Public],
        rpc_args: vec!["--gas-limit".into(), "10000000".into()],
        allow_multiple_solve_requests: false,
        auction_id: 1,
        settle_submission_deadline: 3,
        ..Default::default()
    }
}

#[derive(Debug, Default)]
pub struct Setup {
    name: Option<String>,
    pools: Vec<blockchain::Pool>,
    orders: Vec<Order>,
    order_priority_strategies: Vec<OrderPriorityStrategy>,
    trusted: HashSet<&'static str>,
    config_file: Option<PathBuf>,
    solutions: Vec<Solution>,
    /// Is this a test for the /quote endpoint?
    quote: bool,
    /// List of solvers in this test
    solvers: Vec<Solver>,
    /// Should simulation be enabled? True by default.
    enable_simulation: bool,
    /// Ensure the settlement contract is deployed on a specific address?
    settlement_address: Option<eth::H160>,
    /// Via which mempool the solutions should be submitted
    mempools: Vec<Mempool>,
    /// Extra configuration for the RPC node
    rpc_args: Vec<String>,
    /// List of jit orders returned by the solver
    jit_orders: Vec<JitOrder>,
    /// List of surplus capturing JIT-order owners
    surplus_capturing_jit_order_owners: Vec<H160>,
    /// In case your test requires multiple `/solve` requests
    allow_multiple_solve_requests: bool,
    /// Auction ID used during tests
    auction_id: i64,
    /// The maximum number of blocks to wait for a settlement to appear on
    /// chain.
    settle_submission_deadline: u64,
}

/// The validity of a solution.
#[derive(Debug, Clone, Copy)]
pub enum Calldata {
    /// Set up the solver to return a solution with valid calldata.
    Valid {
        /// Include additional meaningless non-zero bytes appended to the
        /// calldata. This is useful for lowering the solution score in
        /// a controlled way.
        additional_bytes: usize,
    },
    /// Set up the solver to return a solution with bogus calldata.
    Invalid,
}

#[derive(Debug, Clone)]
pub struct Solution {
    pub calldata: Calldata,
    pub orders: Vec<&'static str>,
    pub flashloans: HashMap<order::Uid, Flashloan>,
}

impl Solution {
    /// Reduce the solution score by adding additional meaningless calldata.
    pub fn reduce_score(self) -> Self {
        Self {
            calldata: match self.calldata {
                Calldata::Valid { .. } => Calldata::Valid {
                    additional_bytes: 10,
                },
                Calldata::Invalid => Calldata::Invalid,
            },
            ..self
        }
    }

    /// Increase the solution gas consumption by at least `units`.
    #[allow(dead_code)]
    pub fn increase_gas(self, units: usize) -> Self {
        // non-zero bytes costs 16 gas
        let additional_bytes = (units / 16) + 1;
        Self {
            calldata: match self.calldata {
                Calldata::Valid {
                    additional_bytes: existing,
                } => Calldata::Valid {
                    additional_bytes: existing + additional_bytes,
                },
                Calldata::Invalid => Calldata::Invalid,
            },
            ..self
        }
    }

    /// Make the solution return invalid calldata.
    pub fn invalid(self) -> Self {
        Self {
            calldata: Calldata::Invalid,
            ..self
        }
    }

    pub fn flashloan(mut self, order: order::Uid, flashloan: Flashloan) -> Self {
        self.flashloans.insert(order, flashloan);
        self
    }
}

impl Default for Solution {
    fn default() -> Self {
        Self {
            calldata: Calldata::Valid {
                additional_bytes: 0,
            },
            orders: Default::default(),
            flashloans: Default::default(),
        }
    }
}

/// A pool between tokens "A" and "B".
pub fn ab_pool() -> Pool {
    Pool {
        token_a: "A",
        token_b: "B",
        amount_a: DEFAULT_POOL_AMOUNT_A.ether().into_wei(),
        amount_b: DEFAULT_POOL_AMOUNT_B.ether().into_wei(),
    }
}

pub fn ab_adjusted_pool(quote: LiquidityQuote) -> Pool {
    ab_pool().adjusted_reserve_b(&quote)
}

/// An example order which sells token "A" for token "B".
pub fn ab_order() -> Order {
    Order {
        name: "A-B order",
        sell_amount: AB_ORDER_AMOUNT.ether().into_wei(),
        sell_token: "A",
        buy_token: "B",
        ..Default::default()
    }
}

pub fn ab_liquidity_quote() -> LiquidityQuote {
    LiquidityQuote {
        sell_token: "A",
        buy_token: "B",
        sell_amount: AB_ORDER_AMOUNT.ether().into_wei(),
        buy_amount: 40.ether().into_wei(),
    }
}

/// A solution solving the [`ab_order`].
pub fn ab_solution() -> Solution {
    Solution {
        calldata: Calldata::Valid {
            additional_bytes: 0,
        },
        orders: vec!["A-B order"],
        flashloans: Default::default(),
    }
}

/// An example order which sells token "A" for token "D".
pub fn ad_order() -> Order {
    Order {
        name: "A-D order",
        sell_amount: AD_ORDER_AMOUNT.ether().into_wei(),
        sell_token: "A",
        buy_token: "D",
        ..Default::default()
    }
}

/// A pool between tokens "A" and "D".
pub fn ad_pool() -> Pool {
    Pool {
        token_a: "A",
        token_b: "D",
        amount_a: DEFAULT_POOL_AMOUNT_A.ether().into_wei(),
        amount_b: DEFAULT_POOL_AMOUNT_D.ether().into_wei(),
    }
}

/// A solution solving the [`ad_order`].
pub fn ad_solution() -> Solution {
    Solution {
        calldata: Calldata::Valid {
            additional_bytes: 0,
        },
        orders: vec!["A-D order"],
        flashloans: Default::default(),
    }
}

/// A pool between tokens "C" and "D".
pub fn cd_pool() -> Pool {
    Pool {
        token_a: "C",
        token_b: "D",
        amount_a: DEFAULT_POOL_AMOUNT_C.ether().into_wei(),
        amount_b: DEFAULT_POOL_AMOUNT_D.ether().into_wei(),
    }
}

/// An example order which sells token "C" for token "D".
pub fn cd_order() -> Order {
    Order {
        name: "C-D order",
        sell_amount: CD_ORDER_AMOUNT.ether().into_wei(),
        sell_token: "C",
        buy_token: "D",
        ..Default::default()
    }
}

/// A solution solving the [`cd_order`].
pub fn cd_solution() -> Solution {
    Solution {
        calldata: Calldata::Valid {
            additional_bytes: 0,
        },
        orders: vec!["C-D order"],
        flashloans: Default::default(),
    }
}

/// A pool between "A" and "WETH".
pub fn weth_pool() -> Pool {
    Pool {
        token_a: "A",
        token_b: "WETH",
        amount_a: DEFAULT_POOL_AMOUNT_A.ether().into_wei(),
        amount_b: DEFAULT_POOL_AMOUNT_B.ether().into_wei(),
    }
}

/// An order which buys ETH.
pub fn eth_order() -> Order {
    Order {
        name: "ETH order",
        sell_amount: ETH_ORDER_AMOUNT.ether().into_wei(),
        sell_token: "A",
        buy_token: "ETH",
        ..Default::default()
    }
}

pub fn eth_solution() -> Solution {
    Solution {
        calldata: Calldata::Valid {
            additional_bytes: 0,
        },
        orders: vec!["ETH order"],
        flashloans: Default::default(),
    }
}

// Hardcoded trader account. Don't use this account for anything else!!!
pub const TRADER_ADDRESS: &str = "d2525C68A663295BBE347B65C87c8e17De936a0a";

impl Setup {
    /// Set an explicit name for this test. If a name is set, it will be logged
    /// before the test runs.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Add a uniswap pool with the specified reserves. Tokens are identified
    /// by their symbols. Every order will be solved through one of the pools.
    pub fn pool(mut self, pool: Pool) -> Self {
        self.pools.push(blockchain::Pool {
            reserve_a: Asset {
                token: pool.token_a,
                amount: pool.amount_a,
            },
            reserve_b: Asset {
                token: pool.token_b,
                amount: pool.amount_b,
            },
        });
        self
    }

    /// Add a new order to be solved as part of the test. This order will be
    /// passed to /solve when [`Test::solve`] is called and it will be
    /// anticipated by the mock solver.
    pub fn order(mut self, order: Order) -> Self {
        self.orders.push(order);
        self
    }

    pub fn order_priority_strategy(mut self, strategy: OrderPriorityStrategy) -> Self {
        self.order_priority_strategies.push(strategy);
        self
    }

    /// Set up the protocol to consider the specified token as trusted. The
    /// token is identified by its symbol.
    pub fn trust(mut self, token: &'static str) -> Self {
        self.trusted.insert(token);
        self
    }

    /// Load the specified config file. Otherwise, a temporary config file will
    /// be created with reasonable values.
    pub fn config(mut self, path: PathBuf) -> Self {
        self.config_file = Some(path);
        self
    }

    /// Add a solution to be returned by the mock solver.
    pub fn solution(mut self, solution: Solution) -> Self {
        self.solutions.push(solution);
        self
    }

    pub fn solvers(mut self, solvers: Vec<Solver>) -> Self {
        self.solvers = solvers;
        self
    }

    /// Ensure that the settlement contract is deployed to a specific address.
    pub fn settlement_address(mut self, address: &H160) -> Self {
        self.settlement_address = Some(*address);
        self
    }

    pub fn mempools(mut self, mempools: Vec<Mempool>) -> Self {
        self.mempools = mempools;
        self
    }

    pub fn rpc_args(mut self, rpc_args: Vec<String>) -> Self {
        self.rpc_args = rpc_args;
        self
    }

    pub fn surplus_capturing_jit_order_owners(
        mut self,
        surplus_capturing_jit_order_owners: Vec<H160>,
    ) -> Self {
        self.surplus_capturing_jit_order_owners
            .extend(surplus_capturing_jit_order_owners);
        self
    }

    /// Set specific auction ID which will be used during test.
    /// Setting auction ID can be used to test various scenarios when handling
    /// competing solutions in autopilot, and for interface tests between
    /// autopilot and solvers.
    /// By default auction ID is set to 1.
    pub fn auction_id(mut self, auction_id: i64) -> Self {
        self.auction_id = auction_id;
        self
    }

    /// Set the maximum number of blocks to wait for a settlement to appear on
    /// chain.
    pub fn settle_submission_deadline(mut self, settle_submission_deadline: u64) -> Self {
        self.settle_submission_deadline = settle_submission_deadline;
        self
    }

    /// Create the test: set up onchain contracts and pools, start a mock HTTP
    /// server for the solver and start the HTTP server for the driver.
    pub async fn done(self) -> Test {
        observe::tracing::initialize_reentrant(
            &observe::Config::default()
                .with_env_filter("driver=trace,driver::tests::setup::blockchain=debug,warn"),
        );

        if let Some(name) = self.name.as_ref() {
            tracing::warn!("\n***** [RUNNING TEST CASE] *****\n{name}");
        }

        let deadline = self.deadline();
        let Self {
            pools,
            orders,
            trusted,
            config_file,
            jit_orders,
            surplus_capturing_jit_order_owners,
            ..
        } = self;

        // Hardcoded trader account. Don't use this account for anything else!!!
        let trader_address = eth::H160::from_str(TRADER_ADDRESS).unwrap();
        let trader_secret_key = SecretKey::from_slice(
            &hex::decode("f9f831cee763ef826b8d45557f0f8677b27045e0e011bcd78571a40acc8a6cc3")
                .unwrap(),
        )
        .unwrap();

        // Create the necessary components for testing.
        let blockchain = Blockchain::new(blockchain::Config {
            pools,
            main_trader_secret_key: trader_secret_key,
            solvers: self.solvers.clone(),
            settlement_address: self.settlement_address,
            rpc_args: self.rpc_args,
        })
        .await;
        let mut solutions = Vec::with_capacity(self.solutions.len());
        for solution in &self.solutions {
            let orders = solution
                .orders
                .iter()
                .flat_map(|solution_order| orders.iter().filter(|o| o.name == *solution_order))
                .cloned()
                .collect::<Vec<_>>();
            let fulfillment_trades = blockchain
                .fulfill(orders.iter(), solution)
                .await
                .into_iter()
                .map(Trade::from_fulfillment)
                .collect::<Vec<_>>();

            let jit_orders = solution
                .orders
                .iter()
                .flat_map(|solution_order| {
                    jit_orders
                        .iter()
                        .filter(|&o| o.order.name == *solution_order)
                        .map(|o| &o.order)
                })
                .collect::<Vec<_>>();
            let jit_trades = blockchain
                .fulfill(jit_orders.into_iter(), solution)
                .await
                .into_iter()
                .map(Trade::from_jit)
                .collect::<Vec<_>>();
            solutions.push(blockchain::Solution {
                trades: [fulfillment_trades, jit_trades].concat(),
                flashloans: solution.flashloans.clone(),
            });
        }
        let orderbook = Orderbook::start(&orders);
        let quotes = orders
            .into_iter()
            .map(|order| blockchain.quote(&order))
            .collect::<Vec<_>>();
        let solvers_with_address = join_all(self.solvers.iter().map(|solver| async {
            let instance = SolverInstance::new(solver::Config {
                blockchain: &blockchain,
                solutions: &solutions,
                trusted: &trusted,
                quoted_orders: &quotes,
                deadline: time::Deadline::new(deadline, solver.timeouts),
                quote: self.quote,
                fee_handler: solver.fee_handler,
                private_key: solver.private_key.clone(),
                expected_surplus_capturing_jit_order_owners: surplus_capturing_jit_order_owners
                    .clone(),
                allow_multiple_solve_requests: self.allow_multiple_solve_requests,
            })
            .await;

            (solver.clone(), instance.addr)
        }))
        .await;
        let driver = Driver::new(
            &driver::Config {
                config_file,
                enable_simulation: self.enable_simulation,
                mempools: self.mempools,
                order_priority_strategies: self.order_priority_strategies,
                orderbook,
            },
            &solvers_with_address,
            &blockchain,
        )
        .await;

        Test {
            blockchain,
            driver,
            client: Default::default(),
            trader_address,
            trades: solutions.into_iter().flat_map(|s| s.trades).collect(),
            trusted,
            deadline,
            settle_submission_deadline: self.settle_submission_deadline,
            quoted_orders: quotes,
            quote: self.quote,
            surplus_capturing_jit_order_owners,
            auction_id: self.auction_id,
        }
    }

    /// This is a test for the /quote endpoint.
    pub fn quote(self) -> Self {
        Self {
            quote: true,
            ..self
        }
    }

    /// Solver send the solution as JIT order
    pub fn jit_order(mut self, jit_order: JitOrder) -> Self {
        self.jit_orders.push(jit_order);
        self
    }

    fn deadline(&self) -> chrono::DateTime<chrono::Utc> {
        crate::infra::time::now() + chrono::Duration::seconds(2)
    }

    pub fn allow_multiple_solve_requests(mut self) -> Self {
        self.allow_multiple_solve_requests = true;
        self
    }
}

pub struct Test {
    quoted_orders: Vec<blockchain::QuotedOrder>,
    blockchain: Blockchain,
    driver: Driver,
    client: reqwest::Client,
    trader_address: eth::H160,
    trades: Vec<Trade>,
    trusted: HashSet<&'static str>,
    deadline: chrono::DateTime<chrono::Utc>,
    settle_submission_deadline: u64,
    /// Is this testing the /quote endpoint?
    quote: bool,
    /// List of surplus capturing JIT-order owners
    surplus_capturing_jit_order_owners: Vec<H160>,
    auction_id: i64,
}

impl Test {
    /// Call the /solve endpoint.
    pub async fn solve(&self) -> Solve {
        self.solve_with_solver(solver::NAME).await
    }

    pub async fn solve_with_solver(&self, solver: &str) -> Solve {
        let res = self
            .client
            .post(format!("http://{}/{}/solve", self.driver.addr, solver))
            .json(&driver::solve_req(self))
            .send()
            .await
            .unwrap();
        let status = res.status();
        let body = res.text().await.unwrap();
        tracing::debug!(?status, ?body, "got a response from /solve");
        Solve {
            status,
            body,
            trades: &self.trades,
            blockchain: &self.blockchain,
        }
    }

    /// Call the /reveal endpoint.
    pub async fn reveal(&self, solution_id: u64) -> Reveal {
        let res = self
            .client
            .post(format!(
                "http://{}/{}/reveal",
                self.driver.addr,
                solver::NAME
            ))
            .json(&driver::reveal_req(
                solution_id,
                &self.auction_id.to_string(),
            ))
            .send()
            .await
            .unwrap();
        let status = res.status();
        let body = res.text().await.unwrap();
        tracing::debug!(?status, ?body, "got a response from /reveal");
        Reveal { status, body }
    }

    /// Call the /quote endpoint.
    pub async fn quote(&self) -> Quote {
        if !self.quote {
            panic!("called /quote on a test which wasn't configured to test the /quote endpoint");
        }

        let res = self
            .client
            .get(format!(
                "http://{}/{}/quote",
                self.driver.addr,
                solver::NAME
            ))
            .query(&driver::quote_req(self))
            .send()
            .await
            .unwrap();
        let status = res.status();
        let body = res.text().await.unwrap();
        tracing::debug!(?status, ?body, "got a response from /quote");
        Quote {
            trades: &self.trades,
            status,
            body,
            blockchain: &self.blockchain,
        }
    }

    /// Call the /settle endpoint.
    pub async fn settle(&self, solution_id: u64) -> Settle {
        self.settle_with_solver(solver::NAME, solution_id).await
    }

    pub async fn settle_with_solver(&self, solver_name: &str, solution_id: u64) -> Settle {
        let submission_deadline_latest_block: u64 =
            u64::try_from(self.web3().eth().block_number().await.unwrap()).unwrap()
                + self.settle_submission_deadline;
        let old_balances = self.balances().await;
        let res = self
            .client
            .post(format!(
                "http://{}/{}/settle",
                self.driver.addr, solver_name
            ))
            .json(&driver::settle_req(
                submission_deadline_latest_block,
                solution_id,
                &self.auction_id.to_string(),
            ))
            .send()
            .await
            .unwrap();
        let status_code = res.status();
        let settle_status = match status_code {
            StatusCode::OK => SettleStatus::Ok,
            not_ok => SettleStatus::Err {
                status_code: not_ok,
                body: res.text().await.unwrap(),
            },
        };
        tracing::debug!(status=?status_code, "got a response from /settle");
        Settle {
            old_balances,
            status: settle_status,
        }
    }

    async fn balances(&self) -> HashMap<&'static str, eth::U256> {
        let mut balances = HashMap::new();
        for (token, contract) in self.blockchain.tokens.iter() {
            let balance = contract
                .balance_of(self.trader_address)
                .call()
                .await
                .unwrap();
            balances.insert(*token, balance);
        }
        balances.insert(
            "WETH",
            self.blockchain
                .weth
                .balance_of(self.trader_address)
                .call()
                .await
                .unwrap(),
        );
        balances.insert(
            "ETH",
            self.blockchain
                .web3
                .eth()
                .balance(self.trader_address, None)
                .await
                .unwrap(),
        );
        balances
    }

    #[allow(dead_code)]
    pub fn web3(&self) -> &web3::Web3<DynTransport> {
        &self.blockchain.web3
    }

    /// Changes auction ID for current test.
    /// Can be used in autopilot/solver related test cases to
    /// test context changes for competing solutions.
    /// Default value is set by Setup builder.
    pub fn set_auction_id(&mut self, auction_id: i64) {
        self.auction_id = auction_id;
    }

    pub async fn set_auto_mining(&self, enabled: bool) {
        self.blockchain.set_auto_mining(enabled).await
    }
}

/// A /solve response.
pub struct Solve<'a> {
    status: StatusCode,
    body: String,
    trades: &'a [Trade],
    blockchain: &'a Blockchain,
}

pub struct SolveOk<'a> {
    body: String,
    trades: &'a [Trade],
    blockchain: &'a Blockchain,
}

impl<'a> Solve<'a> {
    /// Expect the /solve endpoint to have returned a 200 OK response.
    pub fn ok(self) -> SolveOk<'a> {
        assert_eq!(self.status, hyper::StatusCode::OK);
        SolveOk {
            body: self.body,
            trades: self.trades,
            blockchain: self.blockchain,
        }
    }

    pub fn err(self) -> SolveErr {
        assert_ne!(self.status, hyper::StatusCode::OK);
        SolveErr { body: self.body }
    }
}

impl SolveOk<'_> {
    fn solutions(&self) -> Vec<serde_json::Value> {
        #[derive(serde::Deserialize)]
        struct Body {
            solutions: Vec<serde_json::Value>,
        }
        serde_json::from_str::<Body>(&self.body).unwrap().solutions
    }

    /// Extracts the solution id from the response. Since response can contain
    /// multiple solutions, it takes the id from the first solution.
    pub fn id(&self) -> u64 {
        let solution = self.solution();
        solution
            .get("solutionId")
            .unwrap()
            .as_u64()
            .unwrap()
            .to_owned()
    }

    /// Extracts the first solution from the response. This is expected to be
    /// always valid if there is a valid solution, as we expect from driver to
    /// not send multiple solutions (yet).
    fn solution(&self) -> serde_json::Value {
        let solutions = self.solutions();
        assert_eq!(solutions.len(), 1);
        let solution = solutions[0].clone();
        assert!(solution.is_object());
        // response contains 1 optional field
        assert!((5..=6).contains(&solution.as_object().unwrap().len()));
        solution
    }

    /// Extracts the score from the response. Since response can contain
    /// multiple solutions, it takes the score from the first solution.
    pub fn score(&self) -> eth::U256 {
        let solution = self.solution();
        assert!(solution.get("score").is_some());
        let score = solution.get("score").unwrap().as_str().unwrap();
        eth::U256::from_dec_str(score).unwrap()
    }

    /// Ensures that `/solve` returns no solutions.
    pub fn empty(self) {
        assert!(self.solutions().is_empty());
    }

    /// Check that the solution contains the expected JIT orders.
    pub fn jit_orders(self, jit_orders: &[JitOrder]) -> Self {
        let solution = self.solution();
        assert!(solution.get("orders").is_some());
        let trades = serde_json::from_value::<HashMap<String, serde_json::Value>>(
            solution.get("orders").unwrap().clone(),
        )
        .unwrap();

        // Since JIT orders don't have UID at creation time, we need to search for
        // matching token pair
        for expected in jit_orders.iter() {
            let exist = trades
                .values()
                .any(|trade| self.trade_matches(trade, expected));
            assert!(exist, "JIT order {expected:?} not found");
        }
        self
    }

    /// Find for a JIT order, given specific token pair and buy/sell amount,
    /// return true if the JIT order was found
    fn trade_matches(&self, trade: &serde_json::Value, expected: &JitOrder) -> bool {
        let u256 =
            |value: &serde_json::Value| eth::U256::from_dec_str(value.as_str().unwrap()).unwrap();
        let sell_token = trade.get("sellToken").unwrap().to_string();
        let sell_token = sell_token.trim_matches('"');
        let buy_token = trade.get("buyToken").unwrap().to_string();
        let buy_token = buy_token.trim_matches('"');
        let sell_amount = u256(trade.get("executedSell").unwrap());
        let buy_amount = u256(trade.get("executedBuy").unwrap());

        sell_token == hex_address(self.blockchain.get_token(expected.order.sell_token))
            && buy_token == hex_address(self.blockchain.get_token(expected.order.buy_token))
            && expected.order.expected_amounts.clone().unwrap().sell == sell_amount
            && expected.order.expected_amounts.clone().unwrap().buy == buy_amount
    }

    /// Check that the solution contains the expected orders.
    pub fn orders(self, orders: &[Order]) -> Self {
        let solution = self.solution();
        assert!(solution.get("orders").is_some());
        let trades = serde_json::from_value::<HashMap<String, serde_json::Value>>(
            solution.get("orders").unwrap().clone(),
        )
        .unwrap();

        for (expected, quoted_order) in orders.iter().map(|expected_order| {
            let quoted_order = self
                .trades
                .iter()
                .filter_map(|trade| match trade {
                    Trade::Fulfillment(fulfillment) => Some(&fulfillment.quoted_order),
                    Trade::Jit(_) => None,
                })
                .find(|f| f.order.name == expected_order.name)
                .unwrap_or_else(|| {
                    panic!(
                        "unexpected order {:?}: fulfillment not found in {:?}",
                        expected_order.name, self.trades,
                    )
                });
            (expected_order, quoted_order)
        }) {
            let uid = quoted_order.order_uid(self.blockchain);
            let trade = trades
                .get(&uid.to_string())
                .expect("Didn't find expected trade in solution");
            let u256 = |value: &serde_json::Value| {
                eth::U256::from_dec_str(value.as_str().unwrap()).unwrap()
            };

            let (expected_sell, expected_buy) = match &expected.expected_amounts {
                Some(executed_amounts) => (executed_amounts.sell, executed_amounts.buy),
                None => (quoted_order.sell, quoted_order.buy),
            };
            assert!(is_approximately_equal(
                u256(trade.get("executedSell").unwrap()),
                expected_sell
            ));
            assert!(is_approximately_equal(
                u256(trade.get("executedBuy").unwrap()),
                expected_buy
            ));
        }
        self
    }
}

pub struct SolveErr {
    body: String,
}

impl SolveErr {
    /// Check the kind field in the error response.
    pub fn kind(self, expected_kind: &str) {
        let result: serde_json::Value = serde_json::from_str(&self.body).unwrap();
        assert!(result.is_object());
        assert_eq!(result.as_object().unwrap().len(), 2);
        assert!(result.get("kind").is_some());
        assert!(result.get("description").is_some());
        let kind = result.get("kind").unwrap().as_str().unwrap();
        assert_eq!(kind, expected_kind);
    }
}

/// A /reveal response.
pub struct Reveal {
    status: StatusCode,
    body: String,
}

impl Reveal {
    /// Expect the /reveal endpoint to have returned a 200 OK response.
    pub fn ok(self) -> RevealOk {
        assert_eq!(self.status, hyper::StatusCode::OK);
        RevealOk { body: self.body }
    }

    /// Expect the /reveal endpoint to return a 400 BAD REQUEST response.
    pub fn err(self) -> RevealErr {
        assert!(!self.status.is_success());
        RevealErr { body: self.body }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExpectedOrderAmounts {
    pub sell: eth::U256,
    pub buy: eth::U256,
}

pub struct RevealOk {
    body: String,
}

impl RevealOk {
    pub fn calldata(self) -> Self {
        let result: serde_json::Value = serde_json::from_str(&self.body).unwrap();
        assert!(result.is_object());
        assert_eq!(result.as_object().unwrap().len(), 1);
        let calldata = result.get("calldata").unwrap().as_object().unwrap();
        assert_eq!(calldata.len(), 2);
        assert!(
            !calldata
                .get("internalized")
                .unwrap()
                .as_str()
                .unwrap()
                .is_empty()
        );
        assert!(
            !calldata
                .get("uninternalized")
                .unwrap()
                .as_str()
                .unwrap()
                .is_empty()
        );
        self
    }
}

pub struct RevealErr {
    body: String,
}

impl RevealErr {
    /// Check the kind field in the error response.
    pub fn kind(self, expected_kind: &str) {
        let result: serde_json::Value = serde_json::from_str(&self.body).unwrap();
        assert!(result.is_object());
        assert_eq!(result.as_object().unwrap().len(), 2);
        assert!(result.get("kind").is_some());
        assert!(result.get("description").is_some());
        let kind = result.get("kind").unwrap().as_str().unwrap();
        assert_eq!(kind, expected_kind);
    }
}

/// A /quote response.
pub struct Quote<'a> {
    trades: &'a [Trade],
    status: StatusCode,
    body: String,
    blockchain: &'a Blockchain,
}

impl<'a> Quote<'a> {
    /// Expect the /quote endpoint to have returned a 200 OK response.
    pub fn ok(self) -> QuoteOk<'a> {
        assert_eq!(self.status, hyper::StatusCode::OK);
        QuoteOk {
            trades: self.trades,
            body: self.body,
            blockchain: self.blockchain,
        }
    }
}

pub struct QuoteOk<'a> {
    trades: &'a [Trade],
    body: String,
    blockchain: &'a Blockchain,
}

impl QuoteOk<'_> {
    /// Check that the quote returns the expected amount of tokens. This is
    /// based on the state of the blockchain and the test setup.
    pub fn amount(self) -> Self {
        let quoted_orders = self
            .trades
            .iter()
            .filter_map(|trade| match trade {
                Trade::Fulfillment(fulfillment) => Some(&fulfillment.quoted_order),
                Trade::Jit(_) => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(quoted_orders.len(), 1);

        let quoted_order = quoted_orders[0];
        let sell_token = self.blockchain.get_token(quoted_order.order.sell_token);
        let buy_token = self.blockchain.get_token(quoted_order.order.buy_token);

        let result: serde_json::Value = serde_json::from_str(&self.body).unwrap();
        let clearing_prices = result
            .get("clearingPrices")
            .unwrap()
            .as_object()
            .unwrap()
            .into_iter()
            .map(|(token, price)| (H160::from_str(token).unwrap(), price.as_str().unwrap()))
            .collect::<HashMap<_, _>>();

        let amount = match quoted_order.order.side {
            order::Side::Buy => clearing_prices.get(&buy_token).unwrap(),
            order::Side::Sell => clearing_prices.get(&sell_token).unwrap(),
        };

        let expected = match quoted_order.order.side {
            order::Side::Buy => (quoted_order.sell - quoted_order.order.surplus_fee()).to_string(),
            order::Side::Sell => quoted_order.buy.to_string(),
        };
        assert_eq!(amount, &expected);
        self
    }

    /// Check that the quote returns the expected interactions. This is
    /// based on the state of the blockchain and the test setup.
    pub fn interactions(self) -> Self {
        let interactions = self
            .trades
            .iter()
            .filter_map(|trade| match trade {
                Trade::Fulfillment(fulfillment) => Some(fulfillment.interactions.as_slice()),
                Trade::Jit(_) => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(interactions.len(), 1);

        let interactions = interactions[0];
        let result: serde_json::Value = serde_json::from_str(&self.body).unwrap();
        let result_interactions = result
            .get("interactions")
            .unwrap()
            .as_array()
            .unwrap()
            .to_owned();
        assert_eq!(result_interactions.len(), interactions.len());
        for (interaction, expected) in result_interactions.iter().zip(interactions) {
            let target = interaction.get("target").unwrap().as_str().unwrap();
            let value = interaction.get("value").unwrap().as_str().unwrap();
            let calldata = interaction.get("callData").unwrap().as_str().unwrap();
            assert_eq!(target, format!("0x{}", hex::encode(expected.address)));
            assert_eq!(value, "0");
            assert_eq!(calldata, format!("0x{}", hex::encode(&expected.calldata)));
        }
        self
    }

    /// Checks that quote returns the expected jir orders. This is based on the
    /// test setup.
    pub fn jit_order(self) -> Self {
        let expected_jit_orders = self
            .trades
            .iter()
            .filter_map(|trade| match trade {
                Trade::Fulfillment(_) => None,
                Trade::Jit(jit) => Some(jit),
            })
            .collect::<Vec<_>>();
        assert_eq!(expected_jit_orders.len(), 1);

        let result: serde_json::Value = serde_json::from_str(&self.body).unwrap();
        let jit_orders = result.get("jitOrders").unwrap().as_array().unwrap();
        assert_eq!(jit_orders.len(), 1);
        let expected = expected_jit_orders[0];
        let result_jit_order = jit_orders[0].as_object().unwrap();
        let app_data = result_jit_order.get("appData").unwrap().as_str().unwrap();
        assert_eq!(
            app_data,
            format!(
                "0x{}",
                hex::encode(expected.quoted_order.order.app_data.hash().0.0)
            )
        );

        let result_pre_interactions = result
            .get("preInteractions")
            .unwrap()
            .as_array()
            .unwrap()
            .to_owned();
        assert_eq!(
            result_pre_interactions.len(),
            expected.quoted_order.order.pre_interactions.len()
        );
        for (interaction, expected) in result_pre_interactions
            .iter()
            .zip(&expected.quoted_order.order.pre_interactions)
        {
            let target = interaction.get("target").unwrap().as_str().unwrap();
            let value = interaction.get("value").unwrap().as_str().unwrap();
            let calldata = interaction.get("callData").unwrap().as_str().unwrap();
            assert_eq!(target, format!("0x{}", hex::encode(expected.address)));
            assert_eq!(value, "0");
            assert_eq!(calldata, format!("0x{}", hex::encode(&expected.calldata)));
        }
        self
    }
}

#[serde_as]
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderQuote {
    #[serde_as(as = "HexOrDecimalU256")]
    pub sell_amount: eth::U256,
    #[serde_as(as = "HexOrDecimalU256")]
    pub buy_amount: eth::U256,
    #[serde_as(as = "HexOrDecimalU256")]
    pub fee: eth::U256,
    pub solver: eth::H160,
}

impl OrderQuote {
    pub fn solver(self, solver: eth::H160) -> Self {
        Self { solver, ..self }
    }
}

/// The expected difference between a previous user balance for a certain token
/// and the balance after the settlement has been broadcast.
#[derive(Debug, Clone, Copy)]
pub enum Balance {
    /// The balance should be greater than before.
    Greater,
    /// The balance should be smaller than before by an exact amount.
    SmallerBy(eth::U256),
}

/// A /settle response.
pub struct Settle {
    old_balances: HashMap<&'static str, eth::U256>,
    status: SettleStatus,
}

#[derive(Debug, PartialEq)]
pub enum SettleStatus {
    Ok,
    Err {
        status_code: StatusCode,
        body: String,
    },
}

pub struct SettleOk {
    old_balances: HashMap<&'static str, eth::U256>,
}

pub struct SettleErr {
    body: String,
}

impl Settle {
    /// Expect the /settle endpoint to have returned a 200 OK response.
    pub async fn ok(self) -> SettleOk {
        // Ensure that the response is OK.
        assert_eq!(self.status, SettleStatus::Ok);

        SettleOk {
            old_balances: self.old_balances,
        }
    }

    /// Expect the /settle endpoint to return a 400 BAD REQUEST response.
    pub fn err(self) -> SettleErr {
        match self.status {
            SettleStatus::Err { status_code, body } => {
                assert_eq!(status_code, hyper::StatusCode::BAD_REQUEST);
                SettleErr { body }
            }
            _ => panic!("expected a 400 BAD REQUEST response"),
        }
    }
}

impl SettleOk {
    /// Check that the user balance changed.
    pub async fn balance(self, test: &Test, token: &'static str, balance: Balance) -> SettleOk {
        let new_balances = test.balances().await;
        let new_balance = new_balances.get(token).unwrap();
        let old_balance = self.old_balances.get(token).unwrap();
        match balance {
            Balance::Greater => assert!(new_balance > old_balance),
            Balance::SmallerBy(diff) => assert_eq!(*new_balance, old_balance - diff),
        }
        self
    }

    /// Ensure that the onchain balances changed in accordance with the
    /// [`ab_order`].
    pub async fn ab_order_executed(self, test: &Test) -> SettleOk {
        self.balance(
            test,
            "A",
            Balance::SmallerBy(AB_ORDER_AMOUNT.ether().into_wei()),
        )
        .await
        .balance(test, "B", Balance::Greater)
        .await
    }

    /// Ensure that the onchain balances changed in accordance with the
    /// [`cd_order`].
    pub async fn cd_order_executed(self, test: &Test) -> SettleOk {
        self.balance(
            test,
            "C",
            Balance::SmallerBy(CD_ORDER_AMOUNT.ether().into_wei()),
        )
        .await
        .balance(test, "D", Balance::Greater)
        .await
    }

    /// Ensure that the onchain balances changed in accordance with the
    /// [`eth_order`].
    pub async fn eth_order_executed(self, test: &Test) -> SettleOk {
        self.balance(
            test,
            "A",
            Balance::SmallerBy(ETH_ORDER_AMOUNT.ether().into_wei()),
        )
        .await
        .balance(test, "ETH", Balance::Greater)
        .await
    }
}

impl SettleErr {
    /// Check the kind field in the error response.
    pub fn kind(&self, expected_kind: &str) {
        assert_eq!(self.get_kind(), expected_kind);
    }

    /// Extract the kind field from the error response.
    pub fn get_kind(&self) -> String {
        let result: serde_json::Value = serde_json::from_str(&self.body).unwrap();
        assert!(result.is_object());
        assert_eq!(result.as_object().unwrap().len(), 2);
        assert!(result.get("kind").is_some());
        assert!(result.get("description").is_some());
        result.get("kind").unwrap().as_str().unwrap().to_string()
    }
}
