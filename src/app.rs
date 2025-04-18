use iced::widget::pick_list;
use iced::font::Weight;
use iced::widget::Column;
use iced::{
    widget::{button, column, progress_bar, row, text, text_input},
    Element,Task, Font,
};
use rfd::FileDialog;
use sysinfo::{Disk, Disks};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub enum Message {
	SourceChanged,
	DestChanged(String),
	StartCopy,
	CopyProgress(u64),
	CopyComplete(Result<(), String>),
	Cancel,
}

pub struct IsoMaker {
	source: String,
	dest: String,
	progress: f32,
	total: u64,
	is_copying: bool,
	error: Option<String>,
	cancel_tx: Option<mpsc::Sender<()>>,
}

impl Default for IsoMaker {
	fn default() -> Self {
		Self {
			source: String::new(),
			dest: String::new(),
			progress: 0.,
			total: 0,
			is_copying: false,
			error: None,
			cancel_tx: None,
		}
	}
}

pub fn update(iso_maker: &mut IsoMaker, message: Message) -> Task<Message> {
	match message {
		Message::SourceChanged => {
			let user_home = match dirs::home_dir() {
				Some(home) => home,
				None => dirs::document_dir()
				.unwrap_or_else(|| dirs::public_dir()
					.unwrap()
				),
			};

			let file = FileDialog::new()
				.set_directory(user_home.to_string_lossy().to_string())
				.pick_file();

			match file {
				Some(f) => iso_maker.source = f.to_string_lossy().to_string(),
				None => if iso_maker.source.is_empty() {
					iso_maker.error = Some("Source file picking was cancelled.".to_string());
				},
			}
		}
		Message::DestChanged(device) => {
			println!("Chosen device: {device}");
			iso_maker.dest = device.clone();

			let disk_info = nusb::list_devices().unwrap()
				.find(|device| device.product_string().unwrap().to_string() == device.clone());

			let iface = match disk_info.detach_and_claim_interface() {
				Ok(iface) => iface,
				Err(e) => {
					iso_maker.error = "Could not get access to the USB device.";
					return Task::none();
				}
			}
		},
		Message::StartCopy => {
			if iso_maker.source.is_empty() || iso_maker.dest.is_empty() {
				iso_maker.error = Some("Source and Destination are both required".into());
				return Task::none();
			}

			iso_maker.is_copying = true;
			iso_maker.error = None;

			let (cancel_tx, cancel_rx) = mpsc::channel(1);
			let (progress_tx, mut progress_rx) = mpsc::channel(100);
			iso_maker.cancel_tx = Some(cancel_tx);

			return Task::batch(vec![
				Task::perform({
					copy_with_progress(iso_maker.source.clone(), iso_maker.dest.clone(), cancel_rx, progress_tx)
				}, Message::CopyComplete),
				Task::perform(
					async move {
						let mut last_progress = 0;
						while let Some(bytes) = progress_rx.recv().await {
							last_progress = bytes;
						}
						last_progress
					}, Message::CopyProgress),
			])
		},
		Message::CopyProgress(bytes) => iso_maker.progress = bytes as f32 / iso_maker.total as f32,
		Message::CopyComplete(result) => {
			iso_maker.is_copying = false;
			match result {
				Ok(_) => iso_maker.progress = 1.,
				Err(e) => iso_maker.error = Some(e),
			}
		},
		Message::Cancel => {
			if let Some(tx) = iso_maker.cancel_tx.take() {
				let _ = tx.blocking_send(());
			}

			iso_maker.is_copying = false;
		}
	}

	Task::none()
}

pub fn view(iso_maker: &IsoMaker) -> Element<Message> {
	// Get the USB disk names
	let disks: Vec<String> = nusb::list_devices().unwrap()
		// Get Removeable Storage Devices, which require a product string
		.filter(|device| device.class() == 0 && device.product_string() != None)
		// Double check that this is actually a USB Mass Storage Device
		.filter(|device| {
			match device.interfaces().next() {
				Some(interface) => interface.interface_string() == None,
				None => false,
			}
		})
		// For some reason fingerprint readers get through, so filter them out as well
		.filter(|device| !device.product_string().unwrap().to_string().contains("Fingerprint"))
		// Map the USB device name as a string and put it into the Vec
		.map(|device| device.product_string().unwrap().to_string())
		.collect();

	
	let controls = column![
		text("ISO Maker")
			.size(24)
			.font(Font {
				weight: Weight::Bold,
				..Font::DEFAULT
			}),
		
		row![
			button("Pick Source")
				.on_press(Message::SourceChanged)
				.padding([8, 16]),
				pick_list(disks, Some(iso_maker.dest.clone()), Message::DestChanged)	
		].spacing(20),

		row![
			button("Start")
				.on_press(Message::StartCopy)
				.padding([8, 16]),

			button("Cancel")
				.on_press(Message::Cancel)
				.padding([8, 16]),
		].spacing(20),

		progress_bar(0.0..=1.0, iso_maker.progress)
			.height(20),

		if let Some(err) = &iso_maker.error {
			text(err).color([0.8, 0.2, 0.2])
		} else {
			text(match (iso_maker.is_copying, iso_maker.progress) {
				(true, _) => format!("Copying: {:.1}%", iso_maker.progress * 100.0),
				(false, 1.0) => "Complete!".into(),
				_ => "Ready".into(),
			})
		}
	].spacing(20).padding(20);

	controls.into()
}

pub fn theme(_iso_maker: &IsoMaker) -> iced::Theme {
	iced::Theme::TokyoNight
}

async fn copy_with_progress(
	source: String,
	dest: String,
	mut cancel_rx: mpsc::Receiver<()>,
	progress_tx: mpsc::Sender<u64>,
) -> Result<(), String>
{
	use tokio::fs::File;
	use tokio::io::{AsyncReadExt, AsyncWriteExt};

	let mut src = File::open(&source)
		.await
		.map_err(|e| format!("Source error: {e}"))?;

	let total = src.metadata().await.map_err(|e| format!("Metadata error: {e}"))?.len();
	let mut dest = File::create(&dest)
		.await
		.map_err(|e| format!("Dest error: {e}"))?;

	let mut buffer = vec![0; 4096 * 1024]; // 4MB buffer
	let mut copied = 0;

	loop {
		tokio::select! {
			_ = cancel_rx.recv() => return Err("Cancelled".into()),
			result = src.read(&mut buffer) => {
				let n = result.map_err(|e| format!("Read error: {e}"))?;

				if n == 0 { break; }

				dest.write_all(&buffer[..n])
					.await
					.map_err(|e| format!("Write error: {e}"))?;

				copied += n as u64;
				let progress = copied as f32 / total as f32;
				let _ = progress_tx.send(progress as u64).await;
			}
		}
	}

	Ok(())
}