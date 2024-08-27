use tokio;
use serde::Deserialize;
use std::time::{SystemTime, Duration};
mod matrix;
use matrix::{Matrix, MatrixSettings};
mod solark;
use solark::{Solark, SolarkSettings};
mod influx;
use influx::{Influx, InfluxSettings};
mod alerting;
use alerting::{Alerting, AlertSettings};

type Result<T> = core::result::Result<T, Box<dyn std::error::Error>>;

//***************************** Main ************************************

#[derive(Debug, Deserialize)]
struct Settings {
    influxdb: InfluxSettings, 
    solark: SolarkSettings,
    matrix: MatrixSettings,
    alerting: AlertSettings,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    use std::env;
    let args: Vec<String> = env::args().collect();
    let cfgpath =
        if args.len() < 2 {
            "/etc/solark_monitor.toml"
        } else {
            &args[1]
        };
    println!("Reading config file {cfgpath:?}");
    let cfg = config::Config::builder()
        .add_source(config::File::new(cfgpath, config::FileFormat::Toml))
        .build()?;
    let settings : Settings = cfg.try_deserialize()?;
    println!("config is {settings:?}");
    println!();
    let room_check_secs = settings.matrix.room_check_secs;
    let mut solark = Solark::new(&settings.solark);
    let mut influx = Influx::new(&settings.influxdb);
    let mut alerting = Alerting::new(
        &settings.alerting,
        Matrix::new(&settings.matrix),
        solark.make_metric_lookup())?;

    // NOW we start actually doing stuff
    // The initial connection is to *try* and set things up
    // We'll try and reconnect later if we fail now
    let solarkcf = solark.connect();
    let influxcf = influx.connect();
    let alertingcf = alerting.connect();
    let (solark_res, influx_res, alert_res) = tokio::join!(solarkcf, influxcf, alertingcf);
    for res in [solark_res, influx_res, alert_res] {
        match res {
            Ok(_) => (),
            Err(e) => alerting.error(&format!("Startup Error: {e:?}")).await,
        };
    }
    let mut interval = tokio::time::interval(solark.get_poll_duration());
    let names_list = solark.make_names_list();
    println!("Starting monitoring loop");
    let mut next_room_check = SystemTime::now() + Duration::from_secs(room_check_secs);
    // We need to start out with some values
    loop {
        interval.tick().await;
        let values = match solark.read_all().await {
            Ok(v) => v,
            Err(e) => {
                alerting.error(&format!("Modbus Error: {e:?}")).await;
                solark.disconnect().await?;
                continue;
            }
        };
        // Write the point and alert at the same time
        let writef = influx.write_point(&values, &names_list);
        let alertingf = alerting.check(&values);
        let (write_res, alert_res) = tokio::join!(writef, alertingf);
        match write_res {
            Ok(_) => (),
            Err(e) => {
                alerting.error(&format!("Influx Error: {e:?}")).await;
                influx.disconnect().await?;
            }
        }
        match alert_res {
            Ok(_) => (),
            Err(e) => {
                alerting.error(&format!("Alerting Error: {e:?}")).await;
                alerting.disconnect().await?;
            }
        }
        alerting.gc();
        // TODO: Kinda hacky, this should be a persistant connection and
        // an async callback, but the API for it is weird
        let tn = SystemTime::now();
        if room_check_secs != 0 && tn > next_room_check {
            alerting.check_rooms().await?;
            next_room_check = tn;
        }
    }
}


