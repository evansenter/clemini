def total(basket):
    if not basket:
        return 0

    counts = [0] * 5
    for book in basket:
        counts[book - 1] += 1

    memo = {}
    prices = {
        1: 800,
        2: 1520,
        3: 2160,
        4: 2560,
        5: 3000
    }

    def solve(current_counts):
        current_counts = tuple(sorted([c for c in current_counts if c > 0], reverse=True))
        if not current_counts:
            return 0
        if current_counts in memo:
            return memo[current_counts]

        res = float('inf')
        num_diff_books = len(current_counts)

        for size in range(1, num_diff_books + 1):
            new_counts = list(current_counts)
            for i in range(size):
                new_counts[i] -= 1
            
            res = min(res, prices[size] + solve(tuple(new_counts)))

        memo[current_counts] = res
        return res

    return solve(tuple(counts))
