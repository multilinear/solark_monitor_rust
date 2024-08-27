use serde::Deserialize;
use crate::solark::{RegData};

type Result<T> = core::result::Result<T, Box<dyn std::error::Error>>;

//***************************** Influx **********************************

// This is a workaround.
// ideally we'd just use a BTreeMap<String,String>, but
// the config crate case squashes keys. Doing it this way
// makes the tag a value, rather than a key.
#[derive(Debug, Deserialize, Clone)]
struct SettingsPair {
    key: String,
    val: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct InfluxSettings {
    enable: bool,
    token: String,
    bucket: String,
    org: String,
    url: String,
    tags: Vec<SettingsPair>,
}

pub struct Influx {
    cfg: InfluxSettings,
    client: Option<influxdb2::Client>,
}

impl Influx {
    pub fn new(cfg: &InfluxSettings) -> Self {
        Self{
            cfg: cfg.clone(),
            client: None,
        }
    }
    pub async fn connect(&mut self) -> Result<()> {
      if !self.cfg.enable { return Ok(()); }
      match self.client {
        Some(_) => Ok(()),
        None =>  {
            use influxdb2::Client;
            let cfg = &self.cfg;
            let client = Client::new(&cfg.url, &cfg.org, &cfg.token);
            println!("connected to InfluxDB at {0:?}", self.cfg.url);
            self.client = Some(client);
            Ok(())
          },
      }
    }
    pub async fn disconnect(&mut self) -> Result<()> {
        self.client = None;
        Ok(())
    }
    pub async fn write_point(&mut self, data: &Vec<RegData>, names: &Vec<String>) -> Result<()> {
        if !self.cfg.enable { return Ok(()); }
        // Automatically reconnect if we're not connected
        self.connect().await?;
        // connect either created client, or errored out
        // so unwrap can't fail here
        let client = self.client.as_mut().unwrap();
        // Build up the list of points
        use influxdb2::models::DataPoint;
        let timestamp = chrono::Utc::now().timestamp_nanos_opt().unwrap();
        let cfg = &self.cfg;
        // rust thinks cfg could escape scope, so we have to collect
        // the iterator rather than lazily evaluating.
        let points: Vec<DataPoint> =  std::iter::zip(names.iter(), data.iter()).map(|(metric, datum)| {
            let point = DataPoint::builder(metric);
            cfg.tags.iter().fold(
                match datum {
                    RegData::Bool(v) => 
                        point.tag("units", "Bool")
                        .field("value", if *v {1} else {0}),
                    RegData::Watts(v) =>
                        point.tag("units", "Watts")
                        .field("value", *v as i64),
                    RegData::Percent(v) =>
                        point.tag("units", "Percent")
                        .field("value", *v as i64),
                    RegData::Hz(v) =>
                        point.tag("units", "Hz")
                        .field("value", *v as i64),
                    RegData::Volts(v) =>
                        point.tag("units", "Volts")
                        .field("value", *v as f64 / 10.0),
                }.timestamp(timestamp),
                |p, tag| p.tag(&tag.key, &tag.val)
            ).build().unwrap()
        }).collect();
        client.write(&self.cfg.bucket, futures::stream::iter(points)).await?;
        Ok(())
    }
}

