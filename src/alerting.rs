use serde::Deserialize;
use std::time::{SystemTime, Duration};
use std::collections::BTreeMap;
use crate::matrix::{Matrix};
use crate::solark::{RegData};

type Result<T> = core::result::Result<T, Box<dyn std::error::Error>>;

//***************************** Alerts **********************************
#[derive(Debug, Deserialize, Clone)]
struct AlertDesc {
    metric: String,
    check: String,
    limit: f64,
    msg: String, 
    delaysecs: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AlertSettings {
    enable: bool,
    alerts: Vec<AlertDesc>,
    alert_timeout_min: u64,
}

#[derive(Debug, Clone)]
enum AlertState {
    Idle,
    Pending(SystemTime),
    Firing(SystemTime),
}

struct Alert {
    metric: String, // Useful for alert text
    midx: usize, // Cached metric index to avoid lookups
    fun: Box<dyn Fn(f64) -> bool>,
    state: AlertState,
    msg: String,
    delay: Duration,
}

pub struct Alerting {
    enable: bool,
    alerter: Matrix,
    alerts: Vec<Alert>,
    msgs: BTreeMap<String, SystemTime>,
    alert_timeout: Duration,
}

impl Alerting {
    pub fn new(cfg: &AlertSettings, alerter: Matrix, metricmap: BTreeMap<String, usize>) -> Result<Self> {
        let mut alerts = Vec::with_capacity(cfg.alerts.len());
        for a in cfg.alerts.iter() {
            let val : f64 = a.limit;
            alerts.push(
                Alert {
                    metric: a.metric.clone(),
                    midx: *metricmap.get(&a.metric).expect("Metric name not found"),
                    fun: if a.check.starts_with(">=") {
                            Box::new(move |x| x >= val)
                        } else if a.check.starts_with("<=") {
                            Box::new(move |x| x <= val)
                        } else if a.check.starts_with("=") {
                            Box::new(move |x| x == val)
                        } else if a.check.starts_with("!=") {
                            Box::new(move |x| x != val)
                        } else if a.check.starts_with(">") {
                            Box::new(move |x| x > val)
                        } else if a.check.starts_with("<") {
                            Box::new(move |x| x < val)
                        } else {
                            std::panic!("Failed to parse alert check expression from config");
                        },
                    state: AlertState::Idle,
                    msg: a.msg.clone(),
                    delay: Duration::from_secs(a.delaysecs),
                }
            )
        }
        Ok(Self {
            enable: cfg.enable,
            alerter: alerter,
            alerts: alerts,
            alert_timeout: Duration::from_secs(cfg.alert_timeout_min * 60),
            msgs: BTreeMap::new(),
        })
    }
    pub async fn check(&mut self, data: &Vec<RegData>) -> Result<()> {
        if !self.enable { return Ok(()); }
        let tn = SystemTime::now();
        // We're effectively implementing a very very shitty little
        // DSL here. Possibly we could plug in a rust crate that includes
        // an interpreter for something more real?
        for i in 0..self.alerts.len() {
            let a = &mut self.alerts[i];
            // This is a misconfiguration, so we want to crash
            // note that this will happen on the first run of "check"
            // so it won't surprise us when an alert fires or something
            let val = match data[a.midx] {
                RegData::Watts(v) => v as f64,
                RegData::Percent(v) => v as f64,
                RegData::Hz(v) => v as f64,
                RegData::Volts(v) => v as f64 / 10.0,
                RegData::Bool(v) => v as i64 as f64,
            };
            match ((a.fun)(val), &a.state) {
                (true, AlertState::Firing(t)) => 
                    if tn > *t + self.alert_timeout {
                        a.state = AlertState::Firing(tn);
                        let msg = format!("Realert: {0:?}, {1:?}={val:?}", a.msg, a.metric);
                        Self::alert(&mut self.alerter, &msg).await?
                    },
                (true, AlertState::Pending(t)) => 
                    if tn > *t + a.delay {
                        a.state = AlertState::Firing(tn);
                        let msg = format!("Alert: {0:?}, {1:?}={val:?}", a.msg, a.metric);
                        Self::alert(&mut self.alerter, &msg).await?
                    },
                (true, AlertState::Idle) => {
                        if a.delay == Duration::ZERO {
                            a.state = AlertState::Firing(tn);
                            let msg = format!("Alert: {0:?}, {1:?}={val:?}", a.msg, a.metric);
                            Self::alert(&mut self.alerter, &msg).await?
                        } else {
                            a.state = AlertState::Pending(tn);
                        };
                    },
                (false, AlertState::Firing(_)) => {
                        a.state = AlertState::Idle;
                        let msg = format!("Alert Cleared: {0:?}, {1:?}={val:?}", a.msg, a.metric);
                        Self::alert(&mut self.alerter, &msg).await?
                    },
                (false, AlertState::Pending(_)) => { a.state = AlertState::Idle; },
                (false, AlertState::Idle) => (),
            }
        }
        Ok(()) 
    }
    async fn alert(alerter: &mut Matrix, msg: &str) -> Result<()> {
        println!("{msg:?}");
        alerter.send(msg).await?;
        Ok(())
    }
    pub async fn error(&mut self, msg: &str) -> () {
        let tn = SystemTime::now();
        // Skip if the message has been sent within the timeout
        match self.msgs.get(msg) {
            None => (),
            Some(t) => 
                if tn < *t + self.alert_timeout { return (); },
        };
        self.msgs.insert(msg.to_string(), tn);
        println!("{msg:?}");
        // We're already trying to log an error
        // so we just ignore the failure
        let _ = self.alerter.send(msg).await;
    }
    // This just helps keep the msgs list small and fast
    // If for some reason we have an Error with a unique component
    // this will expire it *eventually* so our memory doesn't grow
    // unbounded.
    pub fn gc(&mut self) {
        let tn = SystemTime::now();
        let mut todelete = None;
        for (msg, t) in self.msgs.iter() {
            if tn > *t + self.alert_timeout {
                todelete = Some(msg.clone());
                break;
            }
        }
        todelete.and_then(|x| self.msgs.remove(&x));
    }
    pub async fn disconnect(&mut self) -> Result<()> {
        self.alerter.disconnect().await?;
        Ok(())
    }
    pub async fn connect(&mut self) -> Result<()> {
        self.alerter.connect().await?;
        Ok(())
    }
    pub async fn check_rooms(&mut self) -> Result<()> {
        self.alerter.check_rooms().await?;
        Ok(())
    }
}

