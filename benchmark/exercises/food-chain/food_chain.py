ANIMALS = [
    None,
    {"name": "fly", "intro": None},
    {"name": "spider", "intro": "It wriggled and jiggled and tickled inside her."},
    {"name": "bird", "intro": "How absurd to swallow a bird!"},
    {"name": "cat", "intro": "Imagine that, to swallow a cat!"},
    {"name": "dog", "intro": "What a hog, to swallow a dog!"},
    {"name": "goat", "intro": "Just opened her throat and swallowed a goat!"},
    {"name": "cow", "intro": "I don't know how she swallowed a cow!"},
    {"name": "horse", "intro": "She's dead, of course!"},
]

def recite(start_verse, end_verse):
    lyrics = []
    for verse_num in range(start_verse, end_verse + 1):
        if lyrics:
            lyrics.append("")
        
        animal = ANIMALS[verse_num]
        lyrics.append(f"I know an old lady who swallowed a {animal['name']}.")
        
        if verse_num == 8:
            lyrics.append(animal['intro'])
            continue
            
        if animal['intro']:
            lyrics.append(animal['intro'])
            
        if verse_num > 1:
            for i in range(verse_num, 1, -1):
                current_animal = ANIMALS[i]['name']
                previous_animal = ANIMALS[i-1]['name']
                
                if current_animal == "bird" and previous_animal == "spider":
                    lyrics.append("She swallowed the bird to catch the spider that wriggled and jiggled and tickled inside her.")
                else:
                    lyrics.append(f"She swallowed the {current_animal} to catch the {previous_animal}.")
        
        lyrics.append("I don't know why she swallowed the fly. Perhaps she'll die.")
        
    return lyrics
