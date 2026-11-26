use std::{collections::VecDeque, sync::Mutex, time::Duration};

use serde_json::{Value, json};
use xbloom_device::{BleTransport, Studio, StudioRecipe};
use xbloom_recipes::{Client, HttpTransport, Recipe, RecipeSource, Region};

#[derive(Default)]
struct MockCloud(Mutex<usize>);

impl HttpTransport for &MockCloud {
    async fn post(&self, url: &str, body: Vec<u8>) -> Result<Vec<u8>, String> {
        let mut request = self.0.lock().unwrap();
        *request += 1;
        let body: Value = serde_json::from_slice(&body).unwrap();
        let response = match *request {
            1 => {
                assert!(url.ends_with("/tMemberLogin.thtml"));
                assert_eq!(body["email"], "test@example.invalid");
                json!({"result": "success", "member": {"tableId": 10}, "token": "dummy-session"})
            }
            2 => {
                assert!(url.ends_with("/tuMyTeaRecipeCreated.tuhtml"));
                assert!(body.as_str().is_some());
                let list: Vec<_> = (1..=100).map(|id| json!({"tableId": id})).collect();
                json!({"result": "success", "list": list})
            }
            3 => json!({"result": "success", "list": [{"tableId": "101", "extension": [1, 2]}]}),
            4 => json!({"result": "success", "list": [{"tableId": 102}]}),
            5 => {
                assert!(url.ends_with("/tuMyRecipeShared.tuhtml"));
                json!({"result": "success", "list": [{"tableId": 101}, {"tableId": 103}]})
            }
            6 => json!({"result": "success", "list": []}),
            7 => {
                assert!(url.ends_with("/tuMyRecipeProduct.tuhtml"));
                assert!(body.as_str().is_some());
                let list: Vec<_> = (103..=202).map(|id| json!({"tableId": id})).collect();
                json!({"result": "success", "list": list})
            }
            8 => {
                assert!(url.ends_with("/tuMyRecipeProduct.tuhtml"));
                json!({"result": "success", "list": [{"tableId": 203}]})
            }
            9 => {
                assert!(url.ends_with("/tuMyRecipeProduct.tuhtml"));
                json!({"result": "success", "list": [{"tableId": 204}]})
            }
            _ => panic!("unexpected request"),
        };
        Ok(serde_json::to_vec(&response).unwrap())
    }
}

#[tokio::test]
async fn export_walks_all_partitions_and_preserves_raw_fields() {
    let server = MockCloud::default();
    let client = Client::new(&server, Region::International);
    let session = client.login("test@example.invalid", "dummy").await.unwrap();
    let recipes = client.all_recipes(&session).await.unwrap();
    assert_eq!(*server.0.lock().unwrap(), 6);
    assert_eq!(recipes.len(), 103);
    assert_eq!(recipes[100].id(), Some(101));
    assert_eq!(recipes[100].0["extension"], json!([1, 2]));
}

#[tokio::test]
async fn export_all_includes_official_pages_and_skips_duplicates() {
    let server = MockCloud::default();
    let client = Client::new(&server, Region::International);
    let session = client.login("test@example.invalid", "dummy").await.unwrap();
    let recipes = client.recipes(&session, RecipeSource::All).await.unwrap();
    assert_eq!(*server.0.lock().unwrap(), 9);
    assert_eq!(recipes.len(), 204);
    assert_eq!(recipes.last().unwrap().id(), Some(204));
}

#[derive(Default)]
struct MockBle {
    commands: Vec<u16>,
    events: VecDeque<Vec<u8>>,
    direct_start: bool,
}

impl BleTransport for MockBle {
    async fn write(&mut self, frame: &[u8]) -> Result<(), String> {
        let command = u16::from_le_bytes([frame[3], frame[4]]);
        self.commands.push(command);
        match command {
            8100 => {
                assert_eq!(
                    frame,
                    [
                        0x58, 1, 1, 0xa4, 0x1f, 20, 0, 0, 0, 1, 185, 0, 0, 0, 1, 0, 0, 0, 0xbd,
                        0xd1
                    ]
                );
                let status = status(1);
                self.events.push_back(status[..7].to_vec());
                self.events.push_back(status[7..].to_vec());
            }
            8001 => self.events.push_back(status(0x1f)),
            8002 => {
                self.events.push_back(status(0x1e));
                if self.direct_start {
                    self.events.push_back(status(0x22));
                }
            }
            40518 => self.events.push_back(status(0x3b)),
            _ => {}
        }
        Ok(())
    }
    async fn delay(&mut self, _: Duration) {}
    async fn notification(&mut self, _: Duration) -> Result<Option<Vec<u8>>, String> {
        Ok(self.events.pop_front())
    }
}

fn status(state: u8) -> Vec<u8> {
    let mut bytes = vec![0x58, 2, 7, 0x57, 0x1f, 13, 0, 0, 0, 0xc1, state];
    let mut crc = 0_u16;
    for byte in &bytes {
        crc ^= u16::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0x8408
            } else {
                crc >> 1
            };
        }
    }
    bytes.extend(crc.to_le_bytes());
    bytes
}

#[tokio::test]
async fn loading_only_stages_recipe_and_start_respects_firmware_state() {
    let recipe: Recipe = serde_json::from_value(json!({
        "adaptedModel": 1, "dose": 15, "cupType": 2, "rpm": 120, "grinderSize": 63,
        "pourList": [
            {"volume": 50, "temperature": 95, "pattern": 3, "pausing": 20, "flowRate": 3.0},
            {"volume": 190, "temperature": 94, "pattern": 2, "pausing": 0, "flowRate": 3.5}
        ]
    }))
    .unwrap();
    let recipe = StudioRecipe::try_from(&recipe).unwrap();
    let mut studio = Studio::new(MockBle::default());
    assert!(studio.start().await.is_err());
    studio.connect().await.unwrap();
    studio.load(&recipe).await.unwrap();
    assert_eq!(
        studio.into_transport().commands,
        [8100, 8022, 8102, 8104, 8001]
    );

    for direct_start in [true, false] {
        let mut studio = Studio::new(MockBle {
            direct_start,
            ..MockBle::default()
        });
        studio.load(&recipe).await.unwrap();
        studio.start().await.unwrap();
        let commands = studio.into_transport().commands;
        assert_eq!(commands[5], 8002);
        assert_eq!(
            &commands[6..],
            if direct_start { &[][..] } else { &[40518][..] }
        );
    }
}
