-- Insère la date du jour (AAAA-MM-JJ) à chaque curseur.
paul.command("exemple.date", "Exemple : insérer la date", function()
    paul.insert(os.date("%Y-%m-%d"))
end)
