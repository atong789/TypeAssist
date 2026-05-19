use std::path::PathBuf;

use schemars::schema_for;
use volatility_map::VolatilityMap;

fn main() -> anyhow::Result<()> {
    let schema = schema_for!(VolatilityMap);
    let json = serde_json::to_string_pretty(&schema)?;

    let out_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schema");
    std::fs::create_dir_all(&out_dir)?;
    let out_path = out_dir.join("volatility-map.v1.json");
    std::fs::write(&out_path, json)?;

    println!("wrote {}", out_path.display());
    Ok(())
}
