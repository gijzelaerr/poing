fn main() {
    println!("{}", makepad_ai_stems::MODEL_ID);
    println!("{}", makepad_ai_beats::MODEL_ID);
    println!("{}", makepad_ai_speech::whisper::accel_backend_name());
}
