use std::collections::HashMap;
use std::io::{BufWriter, Cursor, Read, Seek, SeekFrom, Write};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::error::Error; 
use walkdir::WalkDir;
use clap::{Parser, Subcommand};
use serde::Deserialize; 

#[derive(Parser, Debug)]
#[command(
    version,
    author = "Équipe DRFR", 
    about = "Patcher Deltarune FR.", 
    long_about =
        "Télécharge automatiquement la dernière version du patch FR pour Deltarune.
        Permet également de désinstaller le patch en restaurant les fichiers originaux."
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

// --- Sous-commandes ---
#[derive(Subcommand, Debug)]
enum Command {
    /// Télécharge et installe la dernière version du patch FR.
    Install {
        /// Chemin vers le répertoire contenant Deltarune.exe
        #[arg(short = 'd', long = "game-dir", value_name = "REPERTOIRE_JEU", required = true)]
        game_dir: PathBuf,
    },
    /// Désinstalle le patch et restaure les fichiers anglais.
    Uninstall {
         /// Chemin vers le répertoire contenant Deltarune.exe
        #[arg(short = 'd', long = "game-dir", value_name = "REPERTOIRE_JEU", required = true)]
        game_dir: PathBuf,
    },
}


type PatchIndex = HashMap<String, PlatformInfo>;

#[derive(Deserialize, Debug)]
struct PatchDetail {
    #[serde(rename = "patchPath")]
    patch_path: String, 

    #[serde(rename = "sourcePath")] 
    source_path: String,
}

#[derive(Deserialize, Debug)]
struct PlatformInfo {
    #[serde(rename = "fileUrl")] 
    file_url: String, 
    patchs: Vec<PatchDetail>,
}

fn unzip_file(archive_path: &Path, target_dir: &Path) -> Result<(), Box<dyn Error>> {
    println!("Décompression de {:?} vers {:?}...", archive_path, target_dir);
    let archive_data = std::fs::read(archive_path)?;
    zip_extract::extract(Cursor::new(archive_data), target_dir, false)?;

    println!("Décompression terminée.");
    Ok(())
}

fn calculate_crc32(data: &[u8]) -> u32 {
    let algorithm = crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);
    algorithm.checksum(data)
}

fn can_apply_bps(source_file_path: &Path, patch_file_path: &Path) -> Result<bool, Box<dyn Error>> {
    println!("Vérification de la compatibilité du patch {:?} avec le fichier source {:?}...", patch_file_path, source_file_path);

    // Lit le footer du bps pour récupérer le CRC32 prévu (octets -7 à -11)
    let mut f = File::open(patch_file_path)?;
    f.seek(SeekFrom::End(-12))?;
    let mut buf: [u8; 4] = [0; 4];
    f.read(&mut buf)?;
    let expected_crc = u32::from_le_bytes(buf);


    // Lit le fichier à patcher 
    let source_data = fs::read(source_file_path)
         .map_err(|e| format!("Erreur lecture source {:?}: {}", source_file_path.display(), e))?;

    // Calcule le CRC32 réel du fichier source
    let actual_crc = calculate_crc32(&source_data);
    if actual_crc == expected_crc {
        println!("OK : Le CRC32 du fichier source ({:#010X}) correspond au CRC32 attendu par le patch.", actual_crc);
        Ok(true)
    } else {
        println!("ERREUR : Le CRC32 du fichier source ({:#010X}) ne correspond PAS au CRC32 attendu par le patch ({:#010X}).", actual_crc, expected_crc);
        Ok(false)
    }
}

fn apply_bps(
    source_file_path: &Path,
    patch_file_path: &Path,
    output_file_path: &Path,
) -> Result<(), Box<dyn Error>> {
    let source_data = std::fs::read(&source_file_path)?;
    let patch_data = std::fs::read(&patch_file_path)?;

    let output = flips::BpsPatch::new(patch_data)
        .apply(source_data)
        .map_err(|e| format!("Erreur lors de l'application du patch BPS: {}", e.to_string()))?;
    std::fs::write(&output_file_path, output.to_bytes())?;

    Ok(())
}

fn select_platform(game_dir: &Path) -> String {
    let steam_api_path = game_dir.join("steam_api.dll"); // Chemin attendu : /chemin/vers/Deltarune/steamapi.dll
    if steam_api_path.exists() && steam_api_path.is_file() {
        println!("Fichier steam_api.dll trouvé. Téléchargement du patch Steam.");
        "steam".to_string()
    } else {
        println!("Fichier steam_api.dll non trouvé. Téléchargement du patch Itch (par défaut).");
        "itch".to_string()
    }
}

fn copy_extra_files(extract_dir: &Path, game_dir: &Path) -> Result<(), Box<dyn Error>> {
    println!("\n--- Copie des fichiers supplémentaires (non-BPS) ---\n");

    for entry_result in WalkDir::new(extract_dir).into_iter().filter_map(|e| e.ok()) {
        let path_in_zip = entry_result.path();

        if !path_in_zip.is_file() {
            continue;
        }

        if path_in_zip.extension().map_or(false, |ext| ext == "bps") {
            continue;
        }

        let relative_path = match path_in_zip.strip_prefix(extract_dir) {
            Ok(p) => p,
            Err(_) => {
                eprintln!("ATTENTION : Impossible de déterminer le chemin relatif pour {:?}. Fichier ignoré.", path_in_zip);
                continue;
            }
        };

        let dest_path = game_dir.join(relative_path);
        println!("Copie : {:?} -> {:?}", path_in_zip, dest_path);

        if let Some(dest_parent) = dest_path.parent() {
            if !dest_parent.exists() {
                println!("Création du répertoire parent de destination : {:?}", dest_parent);
                fs::create_dir_all(dest_parent)?; 
            }
        } else {
            eprintln!("ATTENTION : Impossible de déterminer le répertoire parent pour {:?}. Fichier ignoré.", dest_path);
            continue;
        }

        // Création des sauvegardes (renomme fichier en fichier.bak)
        if dest_path.exists() {
             let backup_path = dest_path.with_extension(
                format!("{}.bak", dest_path.extension().unwrap_or_default().to_str().unwrap_or(""))
            );
            println!("Fichier existant trouvé à {:?}. Sauvegardé en {:?}", dest_path, backup_path);

            let _ = fs::remove_file(&backup_path);

            match fs::rename(&dest_path, &backup_path) {
                Ok(_) => println!("Sauvegarde {:?} créée.", backup_path),
                Err(e) => {
                    eprintln!("ERREUR : Impossible de renommer {:?} en {:?}: {}. Copie annulée pour ce fichier.", dest_path, backup_path, e);
                    continue;
                }
            }
        }

        match fs::copy(path_in_zip, &dest_path) {
            Ok(_) => println!("Fichier {:?} copié avec succès.", dest_path),
            Err(e) => {
                eprintln!("ERREUR : Impossible de copier {:?} vers {:?}: {}.", path_in_zip, dest_path, e);
                continue; 
            }
        }
    }

    println!("\n--- Copie des fichiers supplémentaires terminée ---");
    Ok(())
}


fn fetch_patch_index(url: &str) -> Result<PatchIndex, Box<dyn Error>> {
    println!("Téléchargement de l'index des patchs depuis {}...", url);
    let response = reqwest::blocking::get(url)?;

    response.error_for_status_ref()?;

    let index: PatchIndex = response.json::<PatchIndex>()?;
    println!("Index téléchargé et analysé avec succès.");
    Ok(index)
}

fn download_file(url: &str, output_path: &Path) -> Result<(), Box<dyn Error>> {
    println!("Téléchargement de {} vers {:?}...", url, output_path);
    let mut response = reqwest::blocking::get(url)?;

    response.error_for_status_ref()?;

    let output_file = File::create(output_path)?;
    let mut dest_writer = BufWriter::new(output_file);

    response.copy_to(&mut dest_writer)?;

    dest_writer.flush()?;

    println!("Téléchargement de {} terminé.", url);
    Ok(())
}

fn run_install_process(game_dir: &Path) -> Result<(), Box<dyn Error>> {
     if !game_dir.is_dir() {
        return Err(format!("Le chemin fourni {:?} n'est pas un répertoire valide.", game_dir).into());
    }
    println!("Répertoire du jeu choisi : {:?}", game_dir);
    let index_url = "https://deltarune-fr.com/patch-files/linux/patch_index.json";
    let download_dir = PathBuf::from("/tmp/patcher_drfr/");
    std::fs::create_dir_all(&download_dir)?;
    let zip_filename = "patch_download.zip"; 

    let patch_index = fetch_patch_index(index_url)?;

    let platform_key = select_platform(game_dir);

    let platform_info = patch_index.get(&platform_key).ok_or_else(|| {
        format!("Plateforme '{}' non trouvée dans l'index JSON.", platform_key)
    })?;
    let zip_url = &platform_info.file_url;
    println!(
        "URL du patch trouvée pour la plateforme '{}': {}",
        platform_key, zip_url
    );

    let zip_output_path = download_dir.join(zip_filename);

    download_file(zip_url, &zip_output_path)?;

    println!("Le fichier ZIP a été téléchargé ici : {:?}", zip_output_path);

    // Extraction du ZIP 
   let extract_dir = download_dir.join("./patch_files"); 
    println!("Préparation de l'extraction dans : {:?}", extract_dir);
    if extract_dir.exists() {
        println!("Nettoyage du répertoire d'extraction...");
        std::fs::remove_dir_all(&extract_dir)?; 
    }
    std::fs::create_dir_all(&extract_dir)?; 
    unzip_file(&zip_output_path, &extract_dir)?;
    println!("Archive décompressée avec succès dans {:?}", extract_dir);

    println!("\n--- Début de l'application des patchs ---");
    for detail in &platform_info.patchs {
        println!("\nTraitement du patch : '{}' pour le fichier source '{}'", detail.patch_path, detail.source_path);

        let patch_file_path = extract_dir.join(&detail.patch_path);

        let source_file_path = game_dir.join(&detail.source_path);

        if !patch_file_path.exists() {
            eprintln!("ERREUR : Le fichier patch {:?} est introuvable dans l'archive extraite. Passage au suivant.", patch_file_path);
            continue; // Gestion de l'erreur à réétudier, c'est peut-être mieux d'arrêter l'installation entièrement
        }
        if !source_file_path.exists() {
            eprintln!("ERREUR : Le fichier source {:?} est introuvable dans le répertoire du jeu. Passage au suivant.", source_file_path);
            continue; // Idem
        }

        match can_apply_bps(&source_file_path, &patch_file_path) {
            Ok(true) => {
                println!("Préparation de l'application du patch...");
            }
            Ok(false) => {
                return Err(format!("Le fichier source {:?} ne correspond pas au patch {:?}.", source_file_path, patch_file_path).into());
            }
            Err(e) => {
                eprintln!("Erreur lors de la vérification du patch pour {:?}: {}. Arrêt du patcher.", source_file_path, e);
                return Err(e);
            }
        }

        let backup_file_path = source_file_path.with_extension(
            format!("{}.bak", source_file_path.extension().unwrap_or_default().to_str().unwrap_or(""))
        ); 
        println!("Création de la sauvegarde : {:?}", backup_file_path);
        match std::fs::copy(&source_file_path, &backup_file_path) {
             Ok(_) => println!("Sauvegarde créée."),
             Err(e) => {
                eprintln!("ERREUR lors de la création de la sauvegarde {:?} : {}", backup_file_path, e);
                // On décide de continuer quand même ? Ou de s'arrêter ? Pour l'instant on continue.
                // return Err(format!("Impossible de créer la sauvegarde pour {:?}: {}", source_file_path, e).into());
             }
        }


        println!("Application du patch sur : {:?}", source_file_path);
        match apply_bps(&source_file_path, &patch_file_path, &source_file_path) {
            Ok(_) => println!("Patch appliqué avec succès pour : {:?}", source_file_path),
            Err(e) => {
                eprintln!("ERREUR lors de l'application du patch sur {:?} : {}", source_file_path, e);
                // Essaie de restaurer depuis la sauvegarde. Pas sûr que ça soit hyper utile au final.
                eprintln!("Tentative de restauration depuis {:?}", backup_file_path);
                if backup_file_path.exists() {
                     match std::fs::copy(&backup_file_path, &source_file_path) {
                         Ok(_) => eprintln!("Restauration depuis la sauvegarde réussie."),
                         Err(restore_err) => eprintln!("ERREUR CRITIQUE : Impossible de restaurer {:?} depuis la sauvegarde ! Erreur: {}", source_file_path, restore_err),
                     }
                } else {
                     eprintln!("ERREUR CRITIQUE : Sauvegarde {:?} non trouvée, impossible de restaurer.", backup_file_path);
                }
                return Err(e); 
            }
        }
    }
    
    copy_extra_files(&extract_dir, game_dir)?; 

    println!("\n--- Application des patchs terminée ---");

    Ok(())
}

fn run_uninstall_process(game_dir: &Path) -> Result<(), Box<dyn Error>> {
    println!("\n--- Début de la désinstallation du patch ---");
    println!("Répertoire du jeu cible : {:?}", game_dir);

    let mut restored_count = 0;
    let mut error_count = 0;

     if !game_dir.is_dir() {
        return Err(format!("Le répertoire de jeu spécifié {:?} n'existe pas ou n'est pas un répertoire.", game_dir).into());
    }

    for entry_result in WalkDir::new(game_dir).into_iter().filter_map(|e| e.ok()) {
        let bak_path = entry_result.path();

        if !(bak_path.is_file() && bak_path.extension().map_or(false, |ext| ext == "bak")) {
            continue;
        }

        let original_path = bak_path.with_extension("");

        if original_path == bak_path {
             eprintln!("ATTENTION : Impossible de déterminer le nom original pour {:?}. Fichier ignoré.", bak_path);
             error_count += 1;
             continue;
        }

        println!("\nSauvegarde trouvée : {:?}", bak_path);

        if original_path.exists() {
            println!("Suppression du fichier patché actuel : {:?}", original_path);
            match fs::remove_file(&original_path) {
                Ok(_) => { /* Succès */ }
                Err(e) => {
                    eprintln!("ERREUR : Impossible de supprimer {:?}: {}. Annulation pour ce fichier.", original_path, e);
                    error_count += 1;
                    continue; 
                }
            }
        } else {
            println!("Note : Le fichier {:?} n'existait pas (peut-être déjà supprimé?).", original_path);
        }

        println!("Restauration de {:?} -> {:?}", bak_path, original_path);
        match fs::rename(&bak_path, &original_path) {
            Ok(_) => {
                println!("Fichier {:?} restauré avec succès.", original_path);
                restored_count += 1;
            }
            Err(e) => {
                eprintln!("ERREUR : Impossible de renommer {:?} en {:?}: {}. Le fichier .bak est conservé.", bak_path, original_path, e);
                error_count += 1;
                // Le fichier original est supprimé mais le .bak n'a pas pu être renommé... Situation délicate. Pour l'instant je pense laisser comme ça.
            }
        }
    }

    println!("\n--- Désinstallation terminée ---");
    println!("Fichiers restaurés : {}", restored_count);
    if error_count > 0 {
        println!("Erreurs rencontrées : {}", error_count);
        return Err(format!("{} erreurs se sont produites pendant la désinstallation.", error_count).into());
    }

    Ok(())
}

fn main() {
    let args = Args::parse(); 

    let result = match args.command {
        Command::Install { game_dir } => {
            println!("Lancement du processus d'installation pour : {:?}", game_dir);
            run_install_process(&game_dir) 
        }
        Command::Uninstall { game_dir } => {
            println!("Lancement du processus de désinstallation pour : {:?}", game_dir);
            run_uninstall_process(&game_dir)
        }
    };

    if let Err(e) = result {
        eprintln!("\n--- ERREUR ---");
        eprintln!("{}", e);
        let mut source = e.source();
        while let Some(s) = source {
            eprintln!("  causé par: {}", s);
            source = s.source();
        }
        eprintln!("---------------");
        std::process::exit(1);
    } else {
        println!("\nOpération terminée avec succès ! \nBon jeu !");
    }
}